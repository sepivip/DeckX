use std::{
    io::{self, Read, Write},
    path::{Path, PathBuf},
    sync::mpsc::{self, Receiver, TryRecvError},
    time::Duration,
};

use anyhow::{Context, Result, bail, ensure};
use eframe::egui::{self, RichText, Ui, vec2};
use serde::{Deserialize, de::DeserializeOwned};
use sha2::{Digest, Sha256};
use url::Url;

use crate::{
    actions::open_http,
    plugins::{self, MAX_ARCHIVE, PluginHost, Staged},
    theme::{
        self, BODY, DETAIL, ERROR, FIELD_HEIGHT, GAP, MINT, MUTED, PAGE_TITLE, SECTION_PADDING,
        TIGHT, WELL,
    },
};

const CATALOG: &str = "https://mp-gateway.elgato.com/products";
const STORE: &str = "https://marketplace.elgato.com";
const PAGE_SIZE: usize = 24;

#[derive(Deserialize)]
struct CatalogPage {
    count: usize,
    results: Vec<Product>,
}

#[derive(Clone, Deserialize)]
struct Product {
    name: String,
    slug: String,
    #[serde(default)]
    org_name: String,
    #[serde(default)]
    org_slug: String,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    support_link: Option<String>,
    #[serde(default)]
    additional_links: Vec<ProductLink>,
    #[serde(default)]
    variants: Vec<Variant>,
    #[serde(default)]
    tagsets: Vec<TagSet>,
}

#[derive(Clone, Deserialize)]
struct ProductLink {
    category: String,
    url: String,
}

#[derive(Clone, Deserialize)]
struct Variant {
    is_free: bool,
    needs_license: bool,
}

#[derive(Clone, Deserialize)]
struct TagSet {
    name: String,
    tags: Vec<String>,
}

impl Product {
    fn listing(&self) -> Result<String> {
        ensure!(
            self.slug.len() <= 250
                && !self.slug.is_empty()
                && self
                    .slug
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-'),
            "Invalid Marketplace listing"
        );
        Ok(format!("{STORE}/product/{}", self.slug))
    }

    fn platforms(&self) -> Option<&[String]> {
        self.tagsets
            .iter()
            .find(|set| set.name == "OS")
            .map(|set| set.tags.as_slice())
    }

    fn supports_windows(&self) -> bool {
        self.platforms()
            .is_none_or(|tags| tags.iter().any(|tag| tag.eq_ignore_ascii_case("Windows")))
    }

    fn public_package_allowed(&self) -> bool {
        self.supports_windows()
            && !self.variants.is_empty()
            && self
                .variants
                .iter()
                .all(|variant| variant.is_free && !variant.needs_license)
    }
}

#[derive(Deserialize)]
struct Release {
    tag_name: String,
    assets: Vec<ReleaseAsset>,
}

#[derive(Clone, Deserialize)]
struct ReleaseAsset {
    name: String,
    browser_download_url: Url,
    size: u64,
    /// GitHub's checksum, such as `sha256:…`, on assets uploaded since mid-2025.
    #[serde(default)]
    digest: Option<String>,
}

#[derive(Clone)]
struct PackageSource {
    repository: String,
    release: String,
    asset: ReleaseAsset,
}

pub(crate) struct Download {
    directory: tempfile::TempDir,
    pub(crate) path: PathBuf,
}

fn http(timeout: Duration) -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_global(Some(timeout))
        .max_redirects(0)
        .http_status_as_error(false)
        .build()
        .into()
}

fn get(agent: &ureq::Agent, url: &Url) -> Result<ureq::http::Response<ureq::Body>> {
    agent
        .get(url.as_str())
        .header("User-Agent", concat!("DeckX/", env!("CARGO_PKG_VERSION")))
        .call()
        .with_context(|| format!("Could not reach {}", url.host_str().unwrap_or("the server")))
}

fn json<T: DeserializeOwned>(response: ureq::http::Response<ureq::Body>) -> Result<T> {
    ensure!(
        response.status().is_success(),
        "Server returned HTTP {}",
        response.status().as_u16()
    );
    const JSON_LIMIT: u64 = 3 * 1024 * 1024;
    let mut bytes = Vec::new();
    response
        .into_body()
        .into_reader()
        .take(JSON_LIMIT + 1)
        .read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() as u64 <= JSON_LIMIT,
        "Catalog response is too large"
    );
    serde_json::from_slice(&bytes).context("Unexpected catalog response")
}

fn catalog(query: &str, offset: usize) -> Result<CatalogPage> {
    let mut url = Url::parse(CATALOG)?;
    url.query_pairs_mut()
        .append_pair("extension", "plugins")
        .append_pair("ordering", "-last_published")
        .append_pair("limit", &PAGE_SIZE.to_string())
        .append_pair("offset", &offset.to_string());
    if !query.is_empty() {
        url.query_pairs_mut().append_pair("search", query);
    }
    json(get(&http(Duration::from_secs(15)), &url)?)
}

pub(crate) fn mcp_search(query: &str, offset: usize) -> Result<serde_json::Value> {
    ensure!(
        query.chars().count() <= 100,
        "Search is limited to 100 characters"
    );
    let page = catalog(query, offset)?;
    let listings = page
        .results
        .iter()
        .map(|product| {
            Ok(
                serde_json::json!({"name":product.name,"publisher":product.org_name,
            "slug":product.slug,"listing":product.listing()?,"windows":product.supports_windows(),
            "can_check_public_package":product.public_package_allowed()}),
            )
        })
        .collect::<Result<Vec<_>>>()?;
    let next = offset.saturating_add(PAGE_SIZE);
    Ok(
        serde_json::json!({"query":query,"offset":offset,"total":page.count,"listings":listings,
        "next_offset":(next < page.count).then_some(next)}),
    )
}

pub(crate) fn mcp_download(query: &str, slug: &str, offset: usize) -> Result<Download> {
    ensure!(
        query.chars().count() <= 100,
        "Search is limited to 100 characters"
    );
    let page = catalog(query, offset)?;
    let product = page
        .results
        .iter()
        .find(|product| product.slug == slug)
        .context("Listing not found on that search page")?;
    let source =
        release_source(product)?.context("No public Windows package found for this listing")?;
    let package = download(source.asset.clone())?;
    // The MCP import extracts the package again; this copy is only checked.
    let staged = plugins::stage(package.directory.path(), &package.path, String::new())?;
    matches_listing(product, &source.repository, staged.name(), staged.author())?;
    Ok(package)
}

/// Downloads and stages a publisher package, then checks it against its listing.
fn fetch(product: &Product, source: &PackageSource, packages: &Path) -> Result<Staged> {
    let package = download(source.asset.clone())?;
    let staged = plugins::stage(
        packages,
        &package.path,
        format!("github.com/{}", source.repository),
    )?;
    matches_listing(product, &source.repository, staged.name(), staged.author())?;
    Ok(staged)
}

/// Elgato's listing data has no plugin UUID, so a package must carry the listing's name, and
/// the listing's publisher must be the manifest author or own the linked repository.
fn matches_listing(product: &Product, repository: &str, name: &str, author: &str) -> Result<()> {
    let key = |text: &str| {
        text.chars()
            .filter(|c| c.is_alphanumeric())
            .flat_map(char::to_lowercase)
            .collect::<String>()
    };
    let (name, author) = (key(name), key(author));
    let owner = key(repository.split('/').next().unwrap_or_default());
    ensure!(
        !name.is_empty() && name == key(&product.name),
        "The publisher's package is not {}",
        product.name
    );
    ensure!(
        [key(&product.org_name), key(&product.org_slug)]
            .iter()
            .any(|publisher| !publisher.is_empty()
                && (author.contains(publisher.as_str()) || *publisher == owner)),
        "The publisher's package is not from {}",
        product.org_name
    );
    Ok(())
}

fn github_repo(raw: &str) -> Option<String> {
    let url = Url::parse(raw).ok()?;
    if !trusted_https(&url, &["github.com"]) {
        return None;
    }
    let mut parts = url.path_segments()?;
    let owner = parts.next()?;
    let repo = parts.next()?.trim_end_matches(".git");
    let allowed = |part: &str| {
        !part.is_empty()
            && part != "."
            && part != ".."
            && part
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"-_.".contains(&byte))
    };
    (allowed(owner) && allowed(repo)).then(|| format!("{owner}/{repo}"))
}

fn trusted_https(url: &Url, hosts: &[&str]) -> bool {
    url.scheme() == "https"
        && url.host_str().is_some_and(|host| hosts.contains(&host))
        && url.username().is_empty()
        && url.password().is_none()
        && url.port().is_none()
}

fn release_source(product: &Product) -> Result<Option<PackageSource>> {
    if !product.public_package_allowed() {
        return Ok(None);
    }
    let links = product.support_link.iter().map(String::as_str).chain(
        product
            .additional_links
            .iter()
            .filter(|link| {
                matches!(
                    link.category.as_str(),
                    "Support" | "Setup Guide" | "Source Code"
                )
            })
            .map(|link| link.url.as_str()),
    );
    let mut repos = Vec::new();
    for link in links {
        if let Some(repo) = github_repo(link)
            && !repos
                .iter()
                .any(|seen: &String| seen.eq_ignore_ascii_case(&repo))
        {
            repos.push(repo);
        }
    }
    let agent = http(Duration::from_secs(15));
    let mut limited = false;
    for repository in repos.into_iter().take(3) {
        let url = Url::parse(&format!(
            "https://api.github.com/repos/{repository}/releases/latest"
        ))?;
        let response = get(&agent, &url)?;
        let status = response.status().as_u16();
        if !response.status().is_success() {
            // 404 has no release, 301 is a renamed repository and 403 or 429 is rate limiting.
            limited |= matches!(status, 403 | 429);
            continue;
        }
        let release: Release = json(response)?;
        let mut assets = release
            .assets
            .into_iter()
            .filter(|asset| valid_asset(&repository, asset));
        if let Some(asset) = assets.next()
            && assets.next().is_none()
        {
            return Ok(Some(PackageSource {
                repository,
                release: release.tag_name,
                asset,
            }));
        }
    }
    ensure!(!limited, "GitHub is limiting requests. Try again later.");
    Ok(None)
}

fn valid_asset(repo: &str, asset: &ReleaseAsset) -> bool {
    if !trusted_https(&asset.browser_download_url, &["github.com"])
        || asset.size == 0
        || asset.size > MAX_ARCHIVE
        || asset.name.len() > 240
        || !asset
            .name
            .to_ascii_lowercase()
            .ends_with(".streamdeckplugin")
        || !asset
            .name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"-_.".contains(&byte))
    {
        return false;
    }
    let Some(parts) = asset.browser_download_url.path_segments() else {
        return false;
    };
    let parts: Vec<_> = parts.collect();
    parts.len() == 6
        && format!("{}/{}", parts[0], parts[1]).eq_ignore_ascii_case(repo)
        && parts[2..4] == ["releases", "download"]
        && parts[5] == asset.name
}

fn download(source: ReleaseAsset) -> Result<Download> {
    let digest = source
        .digest
        .as_deref()
        .map(|digest| {
            digest
                .strip_prefix("sha256:")
                .filter(|hex| hex.len() == 64)
                .context("The publisher's package has an unsupported checksum")
        })
        .transpose()?;
    // Allow two minutes plus 128 KiB/s, so slow links can finish large packages.
    let agent = http(Duration::from_secs(120 + source.size / (128 * 1024)));
    let mut url = source.browser_download_url;
    const HOSTS: &[&str] = &[
        "github.com",
        "release-assets.githubusercontent.com",
        "objects.githubusercontent.com",
        "github-releases.githubusercontent.com",
    ];
    for _ in 0..5 {
        ensure!(
            trusted_https(&url, HOSTS),
            "Unexpected publisher download address"
        );
        let response = get(&agent, &url)
            .map_err(|_| anyhow::anyhow!("Could not reach the publisher's download server"))?;
        if response.status().is_redirection() {
            let location = response
                .headers()
                .get("location")
                .context("Download redirect has no destination")?;
            url = url
                .join(location.to_str()?)
                .context("Invalid download redirect")?;
            continue;
        }
        ensure!(
            response.status().is_success(),
            "Download failed with HTTP {}",
            response.status().as_u16()
        );
        let directory = tempfile::tempdir()?;
        let path = directory.path().join(source.name);
        let mut file = Hashing {
            inner: std::fs::File::create(&path)?,
            hash: Sha256::new(),
        };
        let size = io::copy(
            &mut response.into_body().into_reader().take(source.size + 1),
            &mut file,
        )?;
        ensure!(
            size == source.size,
            "Download size does not match the publisher's package"
        );
        file.flush()?;
        if let Some(digest) = digest {
            ensure!(
                format!("{:x}", file.hash.finalize()).eq_ignore_ascii_case(digest),
                "Download checksum does not match the publisher's package"
            );
        }
        return Ok(Download { directory, path });
    }
    bail!("Too many download redirects")
}

/// Hashes what it writes.
struct Hashing<W> {
    inner: W,
    hash: Sha256,
}

impl<W: Write> Write for Hashing<W> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let written = self.inner.write(bytes)?;
        self.hash.update(&bytes[..written]);
        Ok(written)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

enum Response {
    Catalog(CatalogPage, String, usize),
    Source(Option<PackageSource>),
    Staged(Box<Staged>),
}

#[derive(Clone, Copy)]
enum Operation {
    Catalog,
    Source,
    Install,
}

impl Operation {
    fn progress(self) -> &'static str {
        match self {
            Self::Catalog => "Loading plugins…",
            Self::Source => "Checking for a DeckX installer…",
            Self::Install => "Downloading plugin…",
        }
    }
}

#[derive(Default)]
pub struct Marketplace {
    query: String,
    page: Option<CatalogPage>,
    search: String,
    offset: usize,
    selected: Option<Product>,
    source: Option<PackageSource>,
    source_checked: bool,
    job: Option<(Operation, Receiver<Result<Response>>)>,
    status: Option<(String, bool)>,
}

impl Marketplace {
    #[cfg(test)]
    pub(crate) fn visual_fixture(view: &str) -> Self {
        let results = [
            "Stopwatch",
            "Sound Deck",
            "Clipboard History",
            "Window Manager",
            "Example plugin with a deliberately long title to verify how a Marketplace card clips its contents",
            "Media Controls",
        ]
        .into_iter()
        .enumerate()
        .map(|(index, name)| {
            serde_json::from_value::<Product>(serde_json::json!({
                "name": name,
                "slug": format!("example-{index}"),
                "org_name": "Example Publisher",
                "description": "Start, stop, and reset a stopwatch from a key on your deck.",
                "tagsets": [{"name": "OS", "tags": ["Windows", "Mac"]}],
            }))
            .unwrap()
        })
        .collect::<Vec<_>>();
        match view {
            "marketplace-results" => Self {
                page: Some(CatalogPage { count: 12, results }),
                ..Self::default()
            },
            "marketplace-detail" => Self {
                selected: results.first().cloned(),
                page: Some(CatalogPage { count: 12, results }),
                source_checked: true,
                source: Some(PackageSource {
                    repository: "example/stopwatch".into(),
                    release: "v1.2.0".into(),
                    asset: ReleaseAsset {
                        name: "com.example.stopwatch.streamDeckPlugin".into(),
                        browser_download_url: Url::parse("https://example.invalid/package")
                            .unwrap(),
                        size: 1234,
                        digest: None,
                    },
                }),
                ..Self::default()
            },
            "marketplace-empty" => Self {
                query: "Unknown plugin".into(),
                search: "Unknown plugin".into(),
                page: Some(CatalogPage {
                    count: 0,
                    results: Vec::new(),
                }),
                ..Self::default()
            },
            "marketplace-error" => Self {
                status: Some(("Marketplace did not respond in time.".into(), true)),
                ..Self::default()
            },
            "marketplace-loading" => Self {
                // The offscreen harness does not poll jobs or access the network.
                job: Some((Operation::Catalog, mpsc::channel().1)),
                ..Self::default()
            },
            _ => unreachable!(),
        }
    }

    fn start(
        &mut self,
        ui: &Ui,
        operation: Operation,
        work: impl FnOnce() -> Result<Response> + Send + 'static,
    ) {
        let (send, receive) = mpsc::channel();
        let ctx = ui.ctx().clone();
        match std::thread::Builder::new()
            .name("deckx-marketplace".into())
            .spawn(move || {
                let _ = send.send(work());
                ctx.request_repaint();
            }) {
            Ok(_) => {
                self.job = Some((operation, receive));
                self.status = None;
            }
            Err(error) => {
                self.status = Some((format!("Could not start the request: {error}"), true))
            }
        }
    }

    pub fn poll(&mut self, host: &mut PluginHost) {
        let Some((_, job)) = &self.job else { return };
        let result = match job.try_recv() {
            Ok(result) => result,
            Err(TryRecvError::Empty) => return,
            Err(TryRecvError::Disconnected) => {
                Err(anyhow::anyhow!("Marketplace request ended unexpectedly"))
            }
        };
        self.job = None;
        match result {
            Ok(Response::Catalog(page, search, offset)) => {
                self.page = Some(page);
                self.search = search;
                self.offset = offset;
            }
            Ok(Response::Source(source)) => {
                self.source = source;
                self.source_checked = true;
            }
            Ok(Response::Staged(staged)) => match host.install(*staged) {
                Ok(name) => {
                    self.status = Some((
                        format!("{name} installed. Enable it from Installed."),
                        false,
                    ))
                }
                Err(error) => self.status = Some((format!("{error:#}"), true)),
            },
            Err(error) => self.status = Some((format!("{error:#}"), true)),
        }
    }

    fn load(&mut self, ui: &Ui, query: String, offset: usize) {
        self.start(ui, Operation::Catalog, move || {
            Ok(Response::Catalog(catalog(&query, offset)?, query, offset))
        });
    }

    pub fn show(&mut self, ui: &mut Ui, host: &PluginHost) {
        if self.page.is_none() && self.job.is_none() && self.status.is_none() {
            self.load(ui, String::new(), 0);
        }
        if let Some(product) = self.selected.clone() {
            self.detail(ui, host, &product);
        } else {
            self.browse(ui);
        }
    }

    fn browse(&mut self, ui: &mut Ui) {
        let mut enter = false;
        let mut submit = false;
        let busy = self.job.is_some();
        theme::split_row(
            ui,
            76.0,
            FIELD_HEIGHT,
            |ui| {
                let field = theme::search(ui, &mut self.query, "Search Elgato Marketplace", 100);
                enter = field.lost_focus() && ui.input(|input| input.key_pressed(egui::Key::Enter));
            },
            |ui| {
                submit = theme::text_button(ui, "Search", 76.0, false, !busy).clicked();
            },
        );
        if !busy && (submit || enter) {
            self.load(ui, self.query.trim().to_owned(), 0);
        }
        ui.separator();
        let busy = self.job.is_some();
        if self
            .page
            .as_ref()
            .is_none_or(|page| page.results.is_empty())
        {
            let (title, detail) = if busy {
                (
                    "Loading plugins…",
                    "Fetching listings from Elgato Marketplace.",
                )
            } else if self.status.as_ref().is_some_and(|(_, error)| *error) {
                ("Couldn't load Marketplace", "Use Search to try again.")
            } else {
                (
                    "No plugins found",
                    "Try a different plugin or publisher name.",
                )
            };
            ui.vertical_centered(|ui| {
                ui.add_space((ui.available_height() - 108.0).max(0.0) * 0.5);
                if busy {
                    ui.spinner();
                    ui.add_space(TIGHT);
                }
                theme::empty_state(ui, title, detail);
                self.feedback(ui, false);
                if !busy
                    && !self.search.is_empty()
                    && self.status.is_none()
                    && theme::action_button(ui, "Clear search", false, true).clicked()
                {
                    self.query.clear();
                    self.load(ui, String::new(), 0);
                }
            });
            return;
        }
        self.feedback(ui, true);

        let Some(page) = &self.page else { return };
        let end = self.offset + page.results.len();
        let count = page.count;
        let mut picked = None;
        egui::ScrollArea::vertical()
            .id_salt(("marketplace-results", &self.search, self.offset))
            .max_height((ui.available_height() - 47.0).max(64.0))
            .auto_shrink([false, false])
            .show(ui, |ui| {
                for product in &page.results {
                    let publisher = if product.org_name.is_empty() {
                        "Publisher not listed"
                    } else {
                        &product.org_name
                    };
                    ui.push_id(&product.slug, |ui| {
                        theme::list_card(ui, 76.0, &product.name, publisher, None, |ui| {
                            if theme::text_button(ui, "Details", 76.0, false, !busy).clicked() {
                                picked = Some(product.clone());
                            }
                        });
                    });
                }
            });

        ui.separator();
        let mut next = None;
        theme::centered_column(ui, 296.0, |ui| {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = GAP;
                if theme::text_button(ui, "Previous", 86.0, false, !busy && self.offset != 0)
                    .clicked()
                {
                    next = Some(self.offset.saturating_sub(PAGE_SIZE));
                }
                let (rect, _) =
                    ui.allocate_exact_size(vec2(108.0, FIELD_HEIGHT), egui::Sense::hover());
                theme::centered_text(
                    ui.painter(),
                    rect.center(),
                    format!("{}–{} of {count}", self.offset + 1, end),
                    egui::FontId::proportional(DETAIL),
                    MUTED,
                );
                if theme::text_button(ui, "Next", 86.0, false, !busy && end < count).clicked() {
                    next = Some(self.offset + PAGE_SIZE);
                }
            });
        });
        if let Some(product) = picked {
            self.selected = Some(product.clone());
            self.source = None;
            self.source_checked = false;
            self.start(ui, Operation::Source, move || {
                Ok(Response::Source(release_source(&product)?))
            });
        } else if let Some(offset) = next {
            self.load(ui, self.search.clone(), offset);
        }
    }

    fn feedback(&self, ui: &mut Ui, show_progress: bool) {
        if show_progress && let Some((operation, _)) = &self.job {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(RichText::new(operation.progress()).color(MUTED));
            });
        }
        if let Some((message, error)) = &self.status {
            theme::clipped_text(ui, message, BODY, if *error { ERROR } else { MINT });
        }
    }

    fn detail(&mut self, ui: &mut Ui, host: &PluginHost, product: &Product) {
        let busy = self.job.is_some();
        let mut back = false;
        theme::split_row(
            ui,
            163.0,
            FIELD_HEIGHT,
            |ui| {
                back = theme::action_button(ui, "‹ Browse plugins", false, !busy).clicked();
            },
            |ui| {
                if theme::action_button(ui, "Open Elgato listing ↗", false, true).clicked()
                    && let Err(error) = product.listing().and_then(|url| open_http(&url))
                {
                    self.status = Some((format!("{error:#}"), true));
                }
            },
        );
        if back {
            self.selected = None;
            self.source = None;
            self.status = None;
            return;
        }
        ui.separator();
        self.feedback(ui, true);
        egui::ScrollArea::vertical()
            .id_salt(("marketplace-detail", &product.slug))
            .auto_shrink([false, true])
            .show(ui, |ui| {
                ui.label(RichText::new(&product.name).font(theme::bold(PAGE_TITLE)));
                if !product.org_name.is_empty() {
                    ui.label(RichText::new(&product.org_name).color(MUTED));
                }
                if let Some(platforms) = product.platforms() {
                    ui.label(theme::label(platforms.join(" · ")));
                }
                if let Some(description) = &product.description {
                    let intro = description
                        .trim()
                        .lines()
                        .take_while(|line| !line.trim().is_empty())
                        .collect::<Vec<_>>()
                        .join(" ");
                    let mut summary: String = intro.chars().take(320).collect();
                    if summary.len() < intro.len() {
                        summary.push('…');
                    }
                    if !summary.is_empty() {
                        ui.label(summary);
                    }
                }
                ui.add_space(GAP);
                theme::card(WELL, SECTION_PADDING).show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    if let Some(source) = &self.source {
                        theme::heading(ui, "Install in DeckX");
                        ui.label(RichText::new(format!("Publisher package · {} · {}", source.repository, source.release)).color(MUTED));
                        let package_id = source.asset.name.rsplit_once('.').map(|(stem, _)| stem);
                        let installed = host.installed().iter().any(|plugin| Some(plugin.id.as_str()) == package_id);
                        let (source, product, packages) = (source.clone(), product.clone(), host.packages());
                        theme::right_aligned_row(ui, FIELD_HEIGHT, |ui| {
                            if installed {
                                ui.label(RichText::new("Installed").color(MINT));
                            } else if theme::action_button(ui, "Install plugin", true, !busy).clicked() {
                                self.start(ui, Operation::Install, move || Ok(Response::Staged(Box::new(fetch(&product, &source, &packages)?))));
                            }
                        });
                    } else if !busy {
                        let (title, detail) = if !product.supports_windows() {
                            ("Windows support unavailable", "This listing does not include Windows support.")
                        } else if self.source_checked {
                            ("Get it on Elgato Marketplace", "Then import the downloaded package or installed folder from the Installed tab.")
                        } else {
                            ("DeckX installer unavailable", "Open the Elgato listing to see other ways to get this plugin.")
                        };
                        theme::heading(ui, title);
                        ui.label(RichText::new(detail).color(MUTED));
                    } else {
                        theme::heading(ui, "DeckX installation");
                        ui.label(RichText::new("Looking for a package from the publisher.").color(MUTED));
                    }
                });
            });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn asset(url: &str, name: &str, size: u64) -> ReleaseAsset {
        ReleaseAsset {
            name: name.into(),
            browser_download_url: Url::parse(url).expect("asset URL"),
            size,
            digest: None,
        }
    }

    fn product(name: &str, org_name: &str, org_slug: &str) -> Product {
        serde_json::from_value(serde_json::json!({
            "name": name, "slug": "listing", "org_name": org_name, "org_slug": org_slug
        }))
        .expect("listing")
    }

    #[test]
    fn github_links_name_one_repository() {
        for (link, repository) in [
            ("https://github.com/owner/repo", "owner/repo"),
            ("https://github.com/owner/repo.git", "owner/repo"),
            ("https://github.com/Owner/re.po_1/issues", "Owner/re.po_1"),
            ("https://github.com/owner/repo#readme", "owner/repo"),
        ] {
            assert_eq!(github_repo(link).as_deref(), Some(repository), "{link}");
        }
        for link in [
            "http://github.com/owner/repo",
            "https://www.github.com/owner/repo",
            "https://github.com.example/owner/repo",
            "https://user@github.com/owner/repo",
            "https://github.com:444/owner/repo",
            "https://github.com/owner",
            "https://github.com/owner/",
            "https://github.com/owner/..",
            "https://github.com/owner/re%2Fpo",
            "https://gitlab.com/owner/repo",
            "not a link",
        ] {
            assert_eq!(github_repo(link), None, "{link}");
        }
    }

    #[test]
    fn trusted_links_are_plain_https_to_listed_hosts() {
        let hosts = &["github.com", "objects.githubusercontent.com"];
        let trusted = |url: &str| trusted_https(&Url::parse(url).expect("URL"), hosts);
        for url in [
            "https://github.com/a",
            "https://objects.githubusercontent.com/a",
            "https://github.com:443/a",
        ] {
            assert!(trusted(url), "{url}");
        }
        for url in [
            "http://github.com/a",
            "https://api.github.com/a",
            "https://github.com.example/a",
            "https://user@github.com/a",
            "https://user:secret@github.com/a",
            "https://github.com:8443/a",
            "ftp://github.com/a",
        ] {
            assert!(!trusted(url), "{url}");
        }
    }

    #[test]
    fn release_assets_must_be_packages_from_the_repository() {
        let url = "https://github.com/owner/repo/releases/download/v1.0/com.x.streamDeckPlugin";
        let name = "com.x.streamDeckPlugin";
        assert!(valid_asset("owner/repo", &asset(url, name, 10)));
        assert!(valid_asset("Owner/Repo", &asset(url, name, 10)));
        for (repository, url, name, size) in [
            ("other/repo", url, name, 10),
            ("owner/repo", url, name, 0),
            ("owner/repo", url, name, MAX_ARCHIVE + 1),
            ("owner/repo", url, "com.y.streamDeckPlugin", 10),
            (
                "owner/repo",
                "https://github.com/owner/repo/releases/download/v1.0/com.x.zip",
                "com.x.zip",
                10,
            ),
            (
                "owner/repo",
                "http://github.com/owner/repo/releases/download/v1.0/com.x.streamDeckPlugin",
                name,
                10,
            ),
            (
                "owner/repo",
                "https://example.com/owner/repo/releases/download/v1.0/com.x.streamDeckPlugin",
                name,
                10,
            ),
            (
                "owner/repo",
                "https://github.com/owner/repo/releases/latest/v1.0/com.x.streamDeckPlugin",
                name,
                10,
            ),
            (
                "owner/repo",
                "https://github.com/owner/repo/releases/download/com.x.streamDeckPlugin",
                name,
                10,
            ),
            (
                "owner/repo",
                "https://github.com/owner/repo/releases/download/v1.0/com%20x.streamDeckPlugin",
                "com x.streamDeckPlugin",
                10,
            ),
        ] {
            assert!(
                !valid_asset(repository, &asset(url, name, size)),
                "{repository} {url} {name} {size}"
            );
        }
    }

    #[test]
    fn packages_must_match_their_listing() {
        let stopwatch = product("Stopwatch Timekeeper", "gabe565", "gabe565");
        // The manifest author is a person, but the publisher owns the linked repository.
        let repository = "gabe565/streamdeck-stopwatch";
        assert!(
            matches_listing(&stopwatch, repository, "Stopwatch Timekeeper", "Gabe Cook").is_ok()
        );
        let scores = product("Live NHL Scores", "ThatSportsGamer", "thatsportsgamer");
        assert!(
            matches_listing(
                &scores,
                "ThatSportsGamer/live-nhl-scores-for-stream-deck",
                "Live NHL Scores",
                "T.J. Lauerman aka ThatSportsGamer"
            )
            .is_ok()
        );
        let govee = product("Govee Light Management", "Felix Geelhaar", "felixgeelhaar");
        assert!(
            matches_listing(
                &govee,
                "fork/govee",
                "Govee Light Management",
                "Felix Geelhaar"
            )
            .is_ok()
        );
        // Another plugin released from a shared support repository.
        let yap = product("Yap", "ChekTek", "chektek");
        assert!(matches_listing(&yap, "ChekTek/support", "Raycast Connect", "ChekTek").is_err());
        // The listed name from someone else's repository.
        let copy = "mallory/streamdeck-stopwatch";
        assert!(matches_listing(&stopwatch, copy, "Stopwatch Timekeeper", "Mallory").is_err());
        assert!(matches_listing(&product("", "", ""), "a/b", "", "").is_err());
    }

    #[test]
    fn downloads_are_hashed_as_they_are_written() -> Result<()> {
        let mut file = Hashing {
            inner: Vec::new(),
            hash: Sha256::new(),
        };
        file.write_all(b"abc")?;
        assert_eq!(file.inner, b"abc");
        assert_eq!(
            format!("{:x}", file.hash.finalize()),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        Ok(())
    }

    #[test]
    #[ignore = "Downloads a public Marketplace publisher package"]
    fn public_marketplace_plugin_installs_disabled() -> Result<()> {
        let product = catalog("Stopwatch Timekeeper", 0)?
            .results
            .into_iter()
            .find(|product| {
                product.slug == "stopwatch-timekeeper-36f571ce-8d79-4256-90fa-66910fb17048"
            })
            .context("Stopwatch Timekeeper was missing from Marketplace search")?;
        let source = release_source(&product)?.context("The publisher package was unavailable")?;
        assert_eq!(source.repository, "gabe565/streamdeck-stopwatch");
        let temporary = tempfile::tempdir()?;
        let mut host = PluginHost::at(temporary.path().join("PluginHost"))?;
        let staged = fetch(&product, &source, &host.packages())?;
        assert_eq!(host.install(staged)?, "Stopwatch Timekeeper");
        let plugin = host
            .installed()
            .pop()
            .context("Plugin was not registered")?;
        assert_eq!(plugin.id, "com.gabe565.stopwatch");
        assert!(!plugin.enabled);
        assert!(host.catalog().iter().any(|action| action.key));
        Ok(())
    }
}
