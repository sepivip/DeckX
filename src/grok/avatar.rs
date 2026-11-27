use std::{
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, Instant, SystemTime},
};

use anyhow::{Context, Result, ensure};
use image::RgbaImage;
use rquickjs::{
    CatchResultExt, Context as JsContext, Function, Module, Runtime,
    loader::{BuiltinLoader, BuiltinResolver},
};

use super::{Bot, avatar_source};
use crate::asar::Archive;

pub(super) const SIZE: u32 = 96;
const ASSETS: &str = "dist/renderer/assets";
const UNSUPPORTED: &str = "This Grok Bot update is not supported yet";
const MODULE_LIMIT: u64 = 2 * 1024 * 1024;
const SCAN_LIMIT: u64 = 64 * 1024 * 1024;
const DEPENDENCY_LIMIT: usize = 16;
/// Loading evaluates Grok's whole common module and builds each bot's motion state.
const LOAD_BUDGET: Duration = Duration::from_secs(5);
// A frame can contain all six SVGs. Grok 1.1's vector renderer does more
// work on curved outlines, and this deadline only limits the avatar worker.
const FRAME_BUDGET: Duration = Duration::from_millis(1_250);
/// Consecutive render failures before DeckX waits for a Grok Bot update.
const RENDER_ATTEMPTS: u32 = 3;
const RENDER_RETRY: Duration = Duration::from_secs(15);
const UPDATE_CHECK: Duration = Duration::from_secs(2);

/// Identifies the installed Grok Bot app bundle by path, modification time, and size.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Bundle {
    path: PathBuf,
    modified: Option<SystemTime>,
    length: u64,
}

impl Bundle {
    pub fn find() -> Result<Self> {
        let path = ["LOCALAPPDATA", "ProgramFiles", "ProgramFiles(x86)"]
            .into_iter()
            .filter_map(std::env::var_os)
            .map(PathBuf::from)
            .flat_map(|root| [root.join("Programs/Grok Bot"), root.join("Grok Bot")])
            .map(|root| root.join("resources/app.asar"))
            .find(|path| path.is_file())
            .context("Grok Bot's app bundle was not found")?;
        let info = fs::metadata(&path)?;
        Ok(Self {
            path,
            modified: info.modified().ok(),
            length: info.len(),
        })
    }
}

/// Checks for installed updates and keeps a working renderer until its
/// replacement can draw an avatar.
#[derive(Default)]
pub(super) struct Engine {
    renderer: Option<Renderer>,
    bundle: Option<Bundle>,
    next_check: Option<Instant>,
    load_error: Option<String>,
    failures: u32,
    retry: Option<Instant>,
}

impl Engine {
    pub fn renderer(&mut self, now: Instant) -> Result<Option<&mut Renderer>> {
        self.renderer_with(now, || Bundle::find().ok())
    }

    fn renderer_with(
        &mut self,
        now: Instant,
        find_bundle: impl FnOnce() -> Option<Bundle>,
    ) -> Result<Option<&mut Renderer>> {
        let mut changed = false;
        if self.next_check.is_none_or(|at| now >= at) {
            let bundle = find_bundle();
            changed = self.next_check.is_none() || bundle != self.bundle;
            self.next_check = Some(now + UPDATE_CHECK);
            if changed {
                self.bundle = bundle;
                self.failures = 0;
                self.retry = None;
            }
        }
        if changed
            || (self.renderer.is_none()
                && self.failures < RENDER_ATTEMPTS
                && self.retry.is_none_or(|at| now >= at))
        {
            let loaded = self
                .bundle
                .as_ref()
                .context("Grok Bot's app bundle was not found")
                .and_then(|bundle| Renderer::from_archive(&bundle.path));
            match loaded {
                Ok(renderer) => {
                    self.renderer = Some(renderer);
                    self.load_error = None;
                }
                Err(error) => {
                    self.failures = RENDER_ATTEMPTS;
                    self.load_error = Some(format!("{error:#}"));
                    return Err(error);
                }
            }
        }
        Ok(self.renderer.as_mut())
    }

    pub fn load_error(&self) -> Option<&str> {
        self.load_error.as_deref()
    }

    pub fn rendered(&mut self) {
        self.failures = 0;
    }

    pub fn failed(&mut self, now: Instant) {
        self.renderer = None;
        self.failures += 1;
        self.retry = Some(now + RENDER_RETRY);
    }

    pub fn is_loaded(&self) -> bool {
        self.renderer.is_some()
    }
}

pub(super) struct Renderer {
    context: JsContext,
    runtime: Runtime,
    input: String,
}

impl Renderer {
    #[cfg(test)]
    pub fn open() -> Result<Self> {
        Self::from_archive(&Bundle::find()?.path)
    }

    fn from_archive(path: &Path) -> Result<Self> {
        let mut archive = Archive::open(path).context("Could not read Grok Bot's app bundle")?;
        let (name, mut source, adapter) = avatar_module(&mut archive)
            .with_context(|| format!("{UNSUPPORTED}. Its avatar renderer changed."))?;
        let (resolver, loader) = module_dependencies(&mut archive, &name, adapter.imports)?;
        if let Some(range) = adapter.geometry_floor {
            source.replace_range(range, "64");
        }
        source.push('\n');
        source.push_str(&adapter.adapter);
        source.push('\n');
        source.push_str(include_str!("motion.js"));
        let runtime = Runtime::new()?;
        runtime.set_memory_limit(64 * 1024 * 1024);
        runtime.set_max_stack_size(512 * 1024);
        // The bundled modules have no browser, filesystem, or network APIs.
        runtime.set_loader(resolver, loader);
        let context = JsContext::full(&runtime)?;
        let mut renderer = Self {
            context,
            runtime,
            input: String::new(),
        };
        renderer.limit(LOAD_BUDGET);
        renderer.context.with(|ctx| {
            let initialize = || -> rquickjs::Result<()> {
                let module = Module::declare(ctx.clone(), format!("assets/{name}"), source)?;
                let (module, ready) = module.eval()?;
                ready.finish::<()>()?;
                let render: Function = module.get("deckxRender")?;
                ctx.globals().set("deckxRender", render)
            };
            initialize().catch(&ctx).map_err(|error| {
                anyhow::anyhow!("Could not initialize Grok's motion engine: {error}")
            })
        })?;
        let check = renderer.render(
            r#"[{"id":"deckx-preview","avatarShape":"blob","avatarColor":"orange"}]"#,
            0.1,
        )?;
        ensure!(
            check.len() == 1 && rasterize(&check[0])?.pixels().any(|pixel| pixel[3] > 0),
            "Grok Bot's motion engine returned no avatar"
        );
        Ok(renderer)
    }

    fn limit(&self, budget: Duration) {
        let start = Instant::now();
        self.runtime
            .set_interrupt_handler(Some(Box::new(move || start.elapsed() > budget)));
    }

    pub fn render(&mut self, bots: &str, seconds: f64) -> Result<Vec<String>> {
        // New bots build their motion state during this call, which takes longer.
        let changed = self.input != bots;
        self.limit(if changed { LOAD_BUDGET } else { FRAME_BUDGET });
        let frames = self.context.with(|ctx| {
            let render = || -> rquickjs::Result<Vec<String>> {
                let function: Function = ctx.globals().get("deckxRender")?;
                function.call((bots, seconds, SIZE))
            };
            render().catch(&ctx).map_err(|error| {
                anyhow::anyhow!("Grok's motion engine could not render a frame: {error}")
            })
        })?;
        if changed {
            bots.clone_into(&mut self.input);
        }
        Ok(frames)
    }
}

pub(super) fn avatar_inputs<'a>(bots: impl Iterator<Item = &'a Bot>) -> String {
    serde_json::to_string(&bots.collect::<Vec<_>>()).expect("avatar inputs are strings and flags")
}

pub(super) fn rasterize(svg: &str) -> Result<Arc<RgbaImage>> {
    ensure!(svg.len() <= 128 * 1024, "Grok avatar frame is too large");
    let tree = resvg::usvg::Tree::from_str(svg, &resvg::usvg::Options::default())?;
    let mut pixmap = resvg::tiny_skia::Pixmap::new(SIZE, SIZE).context("Invalid avatar size")?;
    resvg::render(
        &tree,
        resvg::tiny_skia::Transform::identity(),
        &mut pixmap.as_mut(),
    );
    let mut pixels = pixmap.take();
    for pixel in pixels.chunks_exact_mut(4) {
        let alpha = u32::from(pixel[3]);
        if alpha > 0 && alpha < 255 {
            for channel in &mut pixel[..3] {
                *channel = ((u32::from(*channel) * 255 + alpha / 2) / alpha).min(255) as u8;
            }
        }
    }
    Ok(Arc::new(
        RgbaImage::from_raw(SIZE, SIZE, pixels).context("Invalid avatar pixels")?,
    ))
}

fn avatar_module(archive: &mut Archive) -> Result<(String, String, avatar_source::AvatarSource)> {
    let candidates: Vec<_> = archive
        .files(ASSETS)
        .context("Grok Bot renderer assets are missing")?
        .filter(|name| avatar_source::is_javascript(name))
        .map(str::to_owned)
        .collect();
    let mut total = 0_u64;
    let mut changed = None;
    // Start with the usual shared chunk, then search the other packed
    // modules if the bundler renamed it.
    for preferred in [true, false] {
        let mut found = None;
        for name in candidates
            .iter()
            .filter(|name| name.starts_with("index.eager-common-") == preferred)
        {
            let Some(size) = archive
                .size(ASSETS, name)
                .filter(|&size| size <= MODULE_LIMIT)
            else {
                continue;
            };
            total += size;
            ensure!(
                total <= SCAN_LIMIT,
                "Grok Bot renderer search exceeded its size limit"
            );
            let Ok(source) = String::from_utf8(archive.read(ASSETS, name, MODULE_LIMIT)?) else {
                continue;
            };
            if avatar_source::is_avatar_module(&source) {
                match avatar_source::discover(&source) {
                    Ok(adapter) => {
                        ensure!(found.is_none(), "Ambiguous Grok avatar module");
                        found = Some((name.clone(), source, adapter));
                    }
                    Err(error) => changed = Some(error),
                }
            }
        }
        if let Some(found) = found {
            return Ok(found);
        }
    }
    Err(changed.unwrap_or_else(|| anyhow::anyhow!("Grok Bot's avatar functions are missing")))
}

fn module_dependencies(
    archive: &mut Archive,
    avatar_name: &str,
    mut pending: Vec<String>,
) -> Result<(BuiltinResolver, BuiltinLoader)> {
    let mut resolver = BuiltinResolver::default();
    let mut loader = BuiltinLoader::default();
    let mut seen = BTreeSet::from([avatar_name.to_owned()]);
    while let Some(name) = pending.pop() {
        if seen.insert(name.clone()) {
            ensure!(
                seen.len() <= DEPENDENCY_LIMIT,
                "Too many Grok Bot renderer imports"
            );
            let source = String::from_utf8(archive.read(ASSETS, &name, MODULE_LIMIT)?)?;
            pending.extend(avatar_source::dependencies(&source)?);
            let module = format!("assets/{name}");
            resolver = resolver.with_module(module.clone());
            loader = loader.with_module(module, source);
        }
    }
    Ok((resolver, loader))
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use super::*;

    const BOTS: &str = r#"[{"id":"one","avatarShape":"blob","avatarColor":"orange"}]"#;
    const FIXTURE: &str = r##"
import { paint } from "./boot-a.js";
function resolveBot(bot) {
    return {...bot, shape: bot.avatarShape, color: bot.avatarColor};
}
function toBody(shape) {
    switch (shape) {
        case "blob": return "sphere";
        case "flower": return "starFlower";
        default: return shape;
    }
}
const colors = {
    black: {light: "#000000", dark: "#ffffff"},
    gray: {light: "#888888", dark: "#777777"}
};
const light = {}, dark = {"--sand-bg-orange-bright": paint}, themes = {light, dark};
function getPalette(theme) { return theme; }
function paintChrome(mode) { return getPalette(themes[mode])["--sand-bg-orange-bright"]; }
function appearance(ink) { return {ink, inkPaint: ink, cutout: true}; }
function createMotion({body}) {
    let pos = 0;
    return { advance(step) { pos += 30 * step; }, frame() { return {pos, body}; } };
}
function createState(motion, state, options) { return { advance() {} }; }
function makeSeed(id) { return id.length; }
function createSvg(frame, color, size, id, options) {
    return {tag:"svg", attrs:{xmlns:"http://www.w3.org/2000/svg", width:options.extent,
        height:options.extent, viewBox:"0 0 96 96"},
        children:[{tag:"circle", attrs:{cx: 25 + frame.pos, cy:30, r:15, fill:color.ink},
        children:[]}]};
}
function serializeSvg(node) {
    const attrs = Object.entries(node.attrs).map(([k, v]) => " " + k + '="' + v + '"').join("");
    return "<" + node.tag + attrs + ">" + node.children.map(serializeSvg).join("") + "</" + node.tag + ">";
}
function driveAvatar(bot) {
    const model = createMotion({body:toBody(bot.avatarShape)});
    const state = createState(model, "idle", {isCompact:true, seed:makeSeed(bot.id), isResting:false});
    return createSvg(model.frame(), appearance(paint), 96, bot.id, {displaySize:42, extent:96});
}
"##;

    fn pack(files: &[(&str, &str)]) -> Result<tempfile::NamedTempFile> {
        let mut entries = serde_json::Map::new();
        let mut contents = Vec::new();
        for &(name, source) in files {
            entries.insert(
                name.into(),
                serde_json::json!({
                    "size": source.len(), "offset": contents.len().to_string(),
                }),
            );
            contents.extend_from_slice(source.as_bytes());
        }
        let mut index = serde_json::json!({"files":entries});
        for folder in ASSETS.split('/').rev() {
            index = serde_json::json!({"files":{folder:index}});
        }
        let index = serde_json::to_vec(&index)?;
        let padded = index.len().next_multiple_of(4);
        let mut file = tempfile::NamedTempFile::new()?;
        for value in [4, 8 + padded, 4 + padded, index.len()] {
            file.write_all(&u32::try_from(value)?.to_le_bytes())?;
        }
        file.write_all(&index)?;
        file.write_all(&vec![0; padded - index.len()])?;
        file.write_all(&contents)?;
        Ok(file)
    }

    fn bundle(file: &tempfile::NamedTempFile) -> Bundle {
        let info = fs::metadata(file.path()).unwrap();
        Bundle {
            path: file.path().to_path_buf(),
            modified: info.modified().ok(),
            length: info.len(),
        }
    }

    #[test]
    fn renamed_renderer_and_imports_reload_while_incompatible_updates_keep_animating() -> Result<()>
    {
        let original = pack(&[
            ("index.eager-common-fixture.js", FIXTURE),
            ("boot-a.js", r##"export const paint = "#f20618";"##),
        ])?;
        let incomplete = pack(&[(
            "index.eager-common-fixture.js",
            "export const version = 20;",
        )])?;
        let updated = FIXTURE
            .replace(
                "return {...bot, shape: bot.avatarShape, color: bot.avatarColor};",
                "return {shape: bot.avatarShape ?? 'blob', color: bot.avatarColor};",
            )
            .replace("resolveBot", "futureResolve")
            .replace("toBody", "$v")
            .replace("createState", "assign")
            .replace("createMotion", "make")
            .replace("makeSeed", "$s")
            .replace("createSvg", "p21")
            .replace("serializeSvg", "serialize")
            .replace("boot-a.js", "bundle-z.js");
        let updated = pack(&[
            ("avatar-engine-z.js", &updated),
            ("bundle-z.js", r#"export { paint } from './colors-z.js';"#),
            ("colors-z.js", r##"export const paint = "#06f218";"##),
        ])?;

        let mut engine = Engine::default();
        let now = Instant::now();
        let renderer = engine
            .renderer_with(now, || Some(bundle(&original)))?
            .unwrap();
        let first = renderer.render(BOTS, 0.1)?;
        assert!(first[0].contains("#f20618"));
        assert_ne!(
            rasterize(&first[0])?,
            rasterize(&renderer.render(BOTS, 0.1)?[0])?
        );

        let check = now + UPDATE_CHECK;
        fs::copy(incomplete.path(), original.path())?;
        assert!(
            engine
                .renderer_with(check, || Some(bundle(&original)))
                .is_err()
        );
        assert!(engine.load_error().is_some());
        let retained = engine.renderer_with(check, || unreachable!())?.unwrap();
        assert!(retained.render(BOTS, 0.1)?[0].contains("#f20618"));

        fs::copy(updated.path(), original.path())?;
        let next = engine
            .renderer_with(check + UPDATE_CHECK, || Some(bundle(&original)))?
            .unwrap();
        assert!(next.render(BOTS, 0.1)?[0].contains("#06f218"));
        assert!(engine.load_error().is_none());
        Ok(())
    }

    #[test]
    #[ignore = "requires the installed Grok Bot package"]
    fn installed_engine_produces_distinct_transparent_frames() {
        let mut renderer = match std::env::var_os("DECKX_GROK_TEST_ARCHIVE") {
            Some(path) => Renderer::from_archive(Path::new(&path)),
            None => Renderer::open(),
        }
        .unwrap();
        let inputs = serde_json::json!([
            {"id":"one","avatarShape":"blob","avatarColor":"orange"},
            {"id":"two","avatarShape":"flower","avatarColor":"green"},
            {"id":"three","avatarShape":"star","avatarColor":"blue"},
            {"id":"four","avatarShape":"heart","avatarColor":"magenta"},
            {"id":"five","avatarShape":"squircle","avatarColor":"violet"},
            {"id":"six","avatarShape":"blob","avatarColor":"gray"}
        ])
        .to_string();
        let mut frames = std::array::from_fn::<_, 6, _>(|_| std::collections::HashSet::new());
        let mut slowest = Duration::ZERO;
        for index in 0..60 {
            let started = Instant::now();
            let svgs = renderer
                .render(&inputs, 0.1)
                .unwrap_or_else(|error| panic!("frame {index}: {error:#}"));
            slowest = slowest.max(started.elapsed());
            assert_eq!(svgs.len(), frames.len());
            for (seen, svg) in frames.iter_mut().zip(svgs) {
                let frame = rasterize(&svg).unwrap();
                assert_eq!(frame.as_raw()[3], 0, "the corners must remain transparent");
                assert!(frame.pixels().any(|pixel| pixel[3] > 0), "empty avatar");
                seen.insert(frame.as_raw().clone());
            }
        }
        eprintln!("Longest six-avatar SVG render: {slowest:?}");
        let distinct = frames.map(|seen| seen.len());
        assert!(
            distinct.iter().all(|&count| count > 1),
            "every avatar must animate: {distinct:?}"
        );
    }
}
