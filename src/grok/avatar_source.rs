//! Locates Grok Bot's motion and SVG functions in its compiled JavaScript.
//! The bindings come from the avatar call sites and named properties, which
//! retain their meaning when the bundler renames local variables.

use std::{collections::HashMap, ops::Range};

use anyhow::{Context, Result, ensure};
use tree_sitter::{Node, Parser, Tree};

pub(super) struct AvatarSource {
    pub imports: Vec<String>,
    pub adapter: String,
    pub geometry_floor: Option<Range<usize>>,
}

pub(super) fn is_avatar_module(source: &str) -> bool {
    ["avatarShape", "isCompact", "displaySize", "inkPaint"]
        .iter()
        .all(|field| source.contains(field))
}

pub(super) fn discover(source: &str) -> Result<AvatarSource> {
    let tree = parse(source)?;
    let module = Module::new(source, tree.root_node());
    let driver = module.function("avatar driver", |_, node| {
        let body = module.text(node);
        ["isCompact", "isResting", "displaySize", "extent"]
            .iter()
            .all(|field| body.contains(field))
    })?;
    let driver = module.bindings[driver];
    let (motion_object, state, seed) = module.state_call(driver)?;
    let motion = unique(
        "motion constructor",
        descendants(driver).filter_map(|node| {
            let (name, value) = module.declaration(node)?;
            let (callee, args) = module.call(value)?;
            (name == motion_object && module.property(*args.first()?, "body").is_some())
                .then_some(callee)
        }),
    )?;
    let svg = unique(
        "SVG renderer",
        descendants(driver).filter_map(|node| {
            let (name, args) = module.call(node)?;
            let frame = *args.first()?;
            let method = frame.child_by_field_name("function")?;
            let receiver = method.child_by_field_name("object")?;
            let property = method.child_by_field_name("property")?;
            let options = *args.last()?;
            (module.text(receiver) == motion_object
                && module.text(property) == "frame"
                && module.property(options, "displaySize").is_some()
                && module.property(options, "extent").is_some())
            .then_some(name)
        }),
    )?;
    // Older bundles wrap their resolver to preserve extra shapes. Prefer
    // that public-facing wrapper when it exists.
    let mut wrapped_resolvers = module.functions().filter_map(|(name, node)| {
        (module.text(node).contains("avatarShape")
            && descendants(node).any(|object| {
                module.property(object, "shape").is_some_and(|value| {
                    value
                        .child_by_field_name("property")
                        .is_some_and(|name| module.text(name) == "avatarShape")
                }) && children(object).any(|field| field.kind() == "spread_element")
            }))
        .then_some(name)
    });
    let resolve = if let Some(name) = wrapped_resolvers.next() {
        ensure!(
            wrapped_resolvers.next().is_none(),
            "Ambiguous avatar identity resolver"
        );
        name
    } else {
        module.function("avatar identity resolver", |_, node| {
            let fields = [("shape", "avatarShape"), ("color", "avatarColor")];
            fields
                .iter()
                .all(|(_, input)| module.text(node).contains(input))
                && descendants(node).any(|object| {
                    fields.iter().all(|(key, input)| {
                        module.property(object, key).is_some_and(|value| {
                            descendants(value).any(|part| {
                                part.child_by_field_name("property")
                                    .is_some_and(|property| module.text(property) == *input)
                            })
                        })
                    })
                })
        })?
    };
    let body = module.function("avatar shape mapping", |_, node| {
        let source = module.text(node);
        ["blob", "sphere", "flower", "starFlower"]
            .iter()
            .all(|shape| source.contains(shape))
            && descendants(node).any(|child| child.kind() == "switch_statement")
    })?;
    let appearance = module.function("avatar appearance", |_, node| {
        module.text(node).contains("inkPaint")
            && descendants(node).any(|object| {
                ["inkPaint", "ink", "cutout"]
                    .iter()
                    .all(|key| module.property(object, key).is_some())
            })
    })?;
    let serialize = module.function("SVG serialization", |_, node| {
        let body = module.text(node);
        ["attrs", "tag", "children", "</"]
            .iter()
            .all(|word| body.contains(word))
    })?;
    let (palette, theme) = module.palette()?;
    let colors = unique(
        "avatar color table",
        module.bindings.iter().filter_map(|(&name, &value)| {
            ["black", "gray"]
                .iter()
                .all(|color| {
                    let Some(shades) = module.property(value, color) else {
                        return false;
                    };
                    ["light", "dark"].iter().all(|mode| {
                        module
                            .property(shades, mode)
                            .is_some_and(|shade| module.key(shade).starts_with('#'))
                    })
                })
                .then_some(name)
        }),
    )?;
    ensure!(
        [motion, state, seed, svg, palette].iter().all(|name| module
            .bindings
            .get(name)
            .is_some_and(|node| is_function(*node))),
        "Avatar calls are no longer backed by functions in the same module"
    );

    // The renderer that exposes a direct SVG wrapper also uses a larger
    // geometry cache. Use the same smaller sprite projection as DeckX's
    // previous adapter for that renderer.
    let compact = module.functions().any(|(_, function)| {
        let text = module.text(function);
        text.contains(svg)
            && text.contains(serialize)
            && descendants(function).any(|node| {
                let Some((name, args)) = module.call(node) else {
                    return false;
                };
                name == serialize
                    && args.len() == 1
                    && module
                        .call(args[0])
                        .is_some_and(|(callee, _)| callee == svg)
            })
    });
    let geometry_floor = compact.then(|| module.geometry_floor()).flatten();
    let adapter = format!(
        "const deckxBindings = {{
            resolve: {resolve}, body: {body}, motion: {motion},
            state: {state}, seed: {seed}, appearance: {appearance}, palette: {palette},
            dark: {theme}.dark, colors: {colors}, svg: {svg}, serialize: {serialize}, compact: {compact}
        }};"
    );
    Ok(AvatarSource {
        imports: imports(&tree, source)?,
        adapter,
        geometry_floor,
    })
}

pub(super) fn dependencies(source: &str) -> Result<Vec<String>> {
    imports(&parse(source)?, source)
}

fn parse(source: &str) -> Result<Tree> {
    let mut parser = Parser::new();
    parser.set_language(&tree_sitter_javascript::LANGUAGE.into())?;
    // Some compiler-generated constructs can leave error nodes outside the
    // avatar functions. QuickJS still checks the whole module when it loads.
    parser
        .parse(source, None)
        .context("Could not parse renderer module")
}

fn imports(tree: &Tree, source: &str) -> Result<Vec<String>> {
    children(tree.root_node())
        .filter(|node| matches!(node.kind(), "import_statement" | "export_statement"))
        .filter_map(|node| node.child_by_field_name("source"))
        .map(|name| {
            let path = literal(&source[name.byte_range()]);
            let file = path
                .strip_prefix("./")
                .filter(|name| is_javascript(name))
                .context("Renderer dependency is outside the packed assets")?;
            Ok(file.to_owned())
        })
        .collect()
}

pub(super) fn is_javascript(name: &str) -> bool {
    name.ends_with(".js")
        && name.len() > 3
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"-_.".contains(&byte))
}

fn literal(text: &str) -> &str {
    if text.starts_with(['"', '\'', '`']) && text.len() >= 2 {
        &text[1..text.len() - 1]
    } else {
        text
    }
}

struct Module<'source, 'tree> {
    source: &'source str,
    root: Node<'tree>,
    bindings: HashMap<&'source str, Node<'tree>>,
}

impl<'source, 'tree> Module<'source, 'tree> {
    fn new(source: &'source str, root: Node<'tree>) -> Self {
        let mut module = Self {
            source,
            root,
            bindings: HashMap::new(),
        };
        for node in children(root) {
            let statement = node.child_by_field_name("declaration").unwrap_or(node);
            if statement.kind() == "function_declaration" {
                if !statement.has_error()
                    && let Some(name) = statement.child_by_field_name("name")
                {
                    module.bindings.insert(module.text(name), statement);
                }
            } else if matches!(
                statement.kind(),
                "variable_declaration" | "lexical_declaration"
            ) {
                for entry in children(statement) {
                    if let Some((name, value)) = module.declaration(entry) {
                        module.bindings.insert(name, value);
                    }
                }
            }
        }
        module
    }

    fn text(&self, node: Node<'tree>) -> &'source str {
        &self.source[node.byte_range()]
    }

    fn key(&self, node: Node<'tree>) -> &'source str {
        literal(self.text(node))
    }

    fn declaration(&self, node: Node<'tree>) -> Option<(&'source str, Node<'tree>)> {
        if node.kind() != "variable_declarator" {
            return None;
        }
        let name = node.child_by_field_name("name")?;
        let value = node.child_by_field_name("value")?;
        (name.kind() == "identifier" && !value.has_error()).then_some((self.text(name), value))
    }

    fn call(&self, node: Node<'tree>) -> Option<(&'source str, Vec<Node<'tree>>)> {
        if node.kind() != "call_expression" {
            return None;
        }
        let name = node.child_by_field_name("function")?;
        if name.kind() != "identifier" {
            return None;
        }
        Some((
            self.text(name),
            children(node.child_by_field_name("arguments")?).collect(),
        ))
    }

    fn property(&self, node: Node<'tree>, key: &str) -> Option<Node<'tree>> {
        if node.kind() != "object" {
            return None;
        }
        children(node).find_map(|pair| {
            if pair.kind() == "shorthand_property_identifier" {
                return (self.text(pair) == key).then_some(pair);
            }
            let name = pair.child_by_field_name("key")?;
            (self.key(name) == key).then(|| pair.child_by_field_name("value"))?
        })
    }

    fn functions(&self) -> impl Iterator<Item = (&'source str, Node<'tree>)> {
        self.bindings
            .iter()
            .filter_map(|(&name, &node)| is_function(node).then_some((name, node)))
    }

    fn function(
        &self,
        role: &str,
        matches: impl Fn(&str, Node<'tree>) -> bool,
    ) -> Result<&'source str> {
        unique(
            role,
            self.functions()
                .filter_map(|(name, node)| matches(name, node).then_some(name)),
        )
    }

    fn state_call(
        &self,
        driver: Node<'tree>,
    ) -> Result<(&'source str, &'source str, &'source str)> {
        unique(
            "avatar state constructor",
            descendants(driver).filter_map(|node| {
                let (name, args) = self.call(node)?;
                let options = *args.last()?;
                self.property(options, "isCompact")?;
                self.property(options, "isResting")?;
                let (seed, _) = self.call(self.property(options, "seed")?)?;
                let motion = *args.first()?;
                (motion.kind() == "identifier").then_some((self.text(motion), name, seed))
            }),
        )
    }

    fn palette(&self) -> Result<(&'source str, &'source str)> {
        // Follow the themed CSS lookup back to its palette factory and theme map.
        unique(
            "avatar theme palette",
            descendants(self.root).filter_map(|node| {
                if node.kind() != "subscript_expression" {
                    return None;
                }
                let (palette, args) = self.call(node.child_by_field_name("object")?)?;
                let [theme_at] = args.as_slice() else {
                    return None;
                };
                if theme_at.kind() != "subscript_expression" {
                    return None;
                }
                let theme = self.text(theme_at.child_by_field_name("object")?);
                let map = *self.bindings.get(theme)?;
                for mode in ["light", "dark"] {
                    let value = self.property(map, mode)?;
                    let value = self
                        .bindings
                        .get(self.text(value))
                        .copied()
                        .unwrap_or(value);
                    if value.kind() != "object" {
                        return None;
                    }
                }
                Some((palette, theme))
            }),
        )
    }

    fn geometry_floor(&self) -> Option<Range<usize>> {
        let candidates = self
            .functions()
            .filter(|(_, function)| {
                let text = self.text(*function);
                ["eyes", "zoom", "matrix("]
                    .iter()
                    .all(|word| text.contains(word))
            })
            .flat_map(|(_, function)| descendants(function))
            .filter_map(|node| {
                if node.kind() != "call_expression" {
                    return None;
                }
                let method = node.child_by_field_name("function")?;
                if self.text(method.child_by_field_name("object")?) != "Math"
                    || self.text(method.child_by_field_name("property")?) != "max"
                {
                    return None;
                }
                let arguments = node.child_by_field_name("arguments")?;
                let arg = arguments.named_child(1)?;
                let value = self.bindings.get(self.text(arg)).copied()?;
                (value.kind() == "number"
                    && self
                        .text(value)
                        .parse::<u16>()
                        .is_ok_and(|n| (128..=512).contains(&n)))
                .then_some(value.byte_range())
            });
        unique("sprite geometry resolution", candidates).ok()
    }
}

fn is_function(node: Node<'_>) -> bool {
    matches!(
        node.kind(),
        "function_declaration" | "function_expression" | "arrow_function"
    )
}

fn unique<T: PartialEq>(role: &str, mut candidates: impl Iterator<Item = T>) -> Result<T> {
    let found = candidates
        .next()
        .with_context(|| format!("Missing {role}"))?;
    ensure!(
        candidates.all(|candidate| candidate == found),
        "Ambiguous {role}"
    );
    Ok(found)
}

fn children(node: Node<'_>) -> impl Iterator<Item = Node<'_>> {
    (0..node.named_child_count())
        .filter_map(move |index| node.named_child(index as u32))
        .filter(|node| node.kind() != "comment")
}

fn descendants(node: Node<'_>) -> impl Iterator<Item = Node<'_>> {
    let mut cursor = node.walk();
    std::iter::once(node).chain(std::iter::from_fn(move || {
        if cursor.goto_first_child() {
            return Some(cursor.node());
        }
        loop {
            if cursor.goto_next_sibling() {
                return Some(cursor.node());
            }
            if !cursor.goto_parent() {
                return None;
            }
        }
    }))
}
