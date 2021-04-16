//! Rustdoc's HTML rendering module.
//!
//! This modules contains the bulk of the logic necessary for rendering a
//! rustdoc `clean::Crate` instance to a set of static HTML pages. This
//! rendering process is largely driven by the `format!` syntax extension to
//! perform all I/O into files and streams.
//!
//! The rendering process is largely driven by the `Context` and `Cache`
//! structures. The cache is pre-populated by crawling the crate in question,
//! and then it is shared among the various rendering threads. The cache is meant
//! to be a fairly large structure not implementing `Clone` (because it's shared
//! among threads). The context, however, should be a lightweight structure. This
//! is cloned per-thread and contains information about what is currently being
//! rendered.
//!
//! In order to speed up rendering (mostly because of markdown rendering), the
//! rendering process has been parallelized. This parallelization is only
//! exposed through the `crate` method on the context, and then also from the
//! fact that the shared cache is stored in TLS (and must be accessed as such).
//!
//! In addition to rendering the crate itself, this module is also responsible
//! for creating the corresponding search index and source file renderings.
//! These threads are not parallelized (they haven't been a bottleneck yet), and
//! both occur before the crate is rendered.

crate mod cache;

#[cfg(test)]
mod tests;

mod context;
mod print_item;
mod write_shared;

crate use context::*;
crate use write_shared::FILES_UNVERSIONED;

use std::cell::Cell;
use std::collections::VecDeque;
use std::default::Default;
use std::fmt;
use std::path::PathBuf;
use std::str;
use std::string::ToString;

use itertools::Itertools;
use rustc_ast_pretty::pprust;
use rustc_attr::{Deprecation, StabilityLevel};
use rustc_data_structures::fx::FxHashSet;
use rustc_hir as hir;
use rustc_hir::def::CtorKind;
use rustc_hir::def_id::DefId;
use rustc_hir::Mutability;
use rustc_middle::middle::stability;
use rustc_middle::ty::TyCtxt;
use rustc_span::symbol::{kw, sym, Symbol};
use serde::ser::SerializeSeq;
use serde::{Serialize, Serializer};

use crate::clean::{self, GetDefId, RenderedLink, SelfTy, TypeKind};
use crate::docfs::PathError;
use crate::error::Error;
use crate::formats::cache::Cache;
use crate::formats::item_type::ItemType;
use crate::formats::{AssocItemRender, FormatRenderer, Impl, RenderMode};
use crate::html::escape::Escape;
use crate::html::format::{
    href, print_abi_with_space, print_default_space, print_generic_bounds, print_where_clause,
    Buffer, PrintWithSpace,
};
use crate::html::markdown::{Markdown, MarkdownHtml, MarkdownSummaryLine};

/// A pair of name and its optional document.
crate type NameDoc = (String, Option<String>);

crate fn ensure_trailing_slash(v: &str) -> impl fmt::Display + '_ {
    crate::html::format::display_fn(move |f| {
        if !v.ends_with('/') && !v.is_empty() { write!(f, "{}/", v) } else { f.write_str(v) }
    })
}

// Helper structs for rendering items/sidebars and carrying along contextual
// information

/// Struct representing one entry in the JS search index. These are all emitted
/// by hand to a large JS file at the end of cache-creation.
#[derive(Debug)]
crate struct IndexItem {
    crate ty: ItemType,
    crate name: String,
    crate path: String,
    crate desc: String,
    crate parent: Option<DefId>,
    crate parent_idx: Option<usize>,
    crate search_type: Option<IndexItemFunctionType>,
    crate aliases: Box<[String]>,
}

/// A type used for the search index.
#[derive(Debug)]
crate struct RenderType {
    ty: Option<DefId>,
    idx: Option<usize>,
    name: Option<String>,
    generics: Option<Vec<Generic>>,
}

impl Serialize for RenderType {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        if let Some(name) = &self.name {
            let mut seq = serializer.serialize_seq(None)?;
            if let Some(id) = self.idx {
                seq.serialize_element(&id)?;
            } else {
                seq.serialize_element(&name)?;
            }
            if let Some(generics) = &self.generics {
                seq.serialize_element(&generics)?;
            }
            seq.end()
        } else {
            serializer.serialize_none()
        }
    }
}

/// A type used for the search index.
#[derive(Debug)]
crate struct Generic {
    name: String,
    defid: Option<DefId>,
    idx: Option<usize>,
}

impl Serialize for Generic {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        if let Some(id) = self.idx {
            serializer.serialize_some(&id)
        } else {
            serializer.serialize_some(&self.name)
        }
    }
}

/// Full type of functions/methods in the search index.
#[derive(Debug)]
crate struct IndexItemFunctionType {
    inputs: Vec<TypeWithKind>,
    output: Option<Vec<TypeWithKind>>,
}

impl Serialize for IndexItemFunctionType {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        // If we couldn't figure out a type, just write `null`.
        let mut iter = self.inputs.iter();
        if match self.output {
            Some(ref output) => iter.chain(output.iter()).any(|ref i| i.ty.name.is_none()),
            None => iter.any(|ref i| i.ty.name.is_none()),
        } {
            serializer.serialize_none()
        } else {
            let mut seq = serializer.serialize_seq(None)?;
            seq.serialize_element(&self.inputs)?;
            if let Some(output) = &self.output {
                if output.len() > 1 {
                    seq.serialize_element(&output)?;
                } else {
                    seq.serialize_element(&output[0])?;
                }
            }
            seq.end()
        }
    }
}

#[derive(Debug)]
crate struct TypeWithKind {
    ty: RenderType,
    kind: TypeKind,
}

impl From<(RenderType, TypeKind)> for TypeWithKind {
    fn from(x: (RenderType, TypeKind)) -> TypeWithKind {
        TypeWithKind { ty: x.0, kind: x.1 }
    }
}

impl Serialize for TypeWithKind {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        (&self.ty.name, ItemType::from(self.kind)).serialize(serializer)
    }
}

#[derive(Debug, Clone)]
crate struct StylePath {
    /// The path to the theme
    crate path: PathBuf,
    /// What the `disabled` attribute should be set to in the HTML tag
    crate disabled: bool,
}

thread_local!(crate static CURRENT_DEPTH: Cell<usize> = Cell::new(0));

fn write_srclink(cx: &Context<'_>, item: &clean::Item, buf: &mut Buffer) {
    if let Some(l) = cx.src_href(item) {
        write!(buf, "<a class=\"srclink\" href=\"{}\" title=\"goto source code\">[src]</a>", l)
    }
}

#[derive(Debug, Eq, PartialEq, Hash)]
struct ItemEntry {
    url: String,
    name: String,
}

impl ItemEntry {
    fn new(mut url: String, name: String) -> ItemEntry {
        while url.starts_with('/') {
            url.remove(0);
        }
        ItemEntry { url, name }
    }
}

impl ItemEntry {
    crate fn print(&self) -> impl fmt::Display + '_ {
        crate::html::format::display_fn(move |f| {
            write!(f, "<a href=\"{}\">{}</a>", self.url, Escape(&self.name))
        })
    }
}

impl PartialOrd for ItemEntry {
    fn partial_cmp(&self, other: &ItemEntry) -> Option<::std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for ItemEntry {
    fn cmp(&self, other: &ItemEntry) -> ::std::cmp::Ordering {
        self.name.cmp(&other.name)
    }
}

#[derive(Debug)]
struct AllTypes {
    structs: FxHashSet<ItemEntry>,
    enums: FxHashSet<ItemEntry>,
    unions: FxHashSet<ItemEntry>,
    primitives: FxHashSet<ItemEntry>,
    traits: FxHashSet<ItemEntry>,
    macros: FxHashSet<ItemEntry>,
    functions: FxHashSet<ItemEntry>,
    typedefs: FxHashSet<ItemEntry>,
    opaque_tys: FxHashSet<ItemEntry>,
    statics: FxHashSet<ItemEntry>,
    constants: FxHashSet<ItemEntry>,
    keywords: FxHashSet<ItemEntry>,
    attributes: FxHashSet<ItemEntry>,
    derives: FxHashSet<ItemEntry>,
    trait_aliases: FxHashSet<ItemEntry>,
}

impl AllTypes {
    fn new() -> AllTypes {
        let new_set = |cap| FxHashSet::with_capacity_and_hasher(cap, Default::default());
        AllTypes {
            structs: new_set(100),
            enums: new_set(100),
            unions: new_set(100),
            primitives: new_set(26),
            traits: new_set(100),
            macros: new_set(100),
            functions: new_set(100),
            typedefs: new_set(100),
            opaque_tys: new_set(100),
            statics: new_set(100),
            constants: new_set(100),
            keywords: new_set(100),
            attributes: new_set(100),
            derives: new_set(100),
            trait_aliases: new_set(100),
        }
    }

    fn append(&mut self, item_name: String, item_type: &ItemType) {
        let mut url: Vec<_> = item_name.split("::").skip(1).collect();
        if let Some(name) = url.pop() {
            let new_url = format!("{}/{}.{}.html", url.join("/"), item_type, name);
            url.push(name);
            let name = url.join("::");
            match *item_type {
                ItemType::Struct => self.structs.insert(ItemEntry::new(new_url, name)),
                ItemType::Enum => self.enums.insert(ItemEntry::new(new_url, name)),
                ItemType::Union => self.unions.insert(ItemEntry::new(new_url, name)),
                ItemType::Primitive => self.primitives.insert(ItemEntry::new(new_url, name)),
                ItemType::Trait => self.traits.insert(ItemEntry::new(new_url, name)),
                ItemType::Macro => self.macros.insert(ItemEntry::new(new_url, name)),
                ItemType::Function => self.functions.insert(ItemEntry::new(new_url, name)),
                ItemType::Typedef => self.typedefs.insert(ItemEntry::new(new_url, name)),
                ItemType::OpaqueTy => self.opaque_tys.insert(ItemEntry::new(new_url, name)),
                ItemType::Static => self.statics.insert(ItemEntry::new(new_url, name)),
                ItemType::Constant => self.constants.insert(ItemEntry::new(new_url, name)),
                ItemType::ProcAttribute => self.attributes.insert(ItemEntry::new(new_url, name)),
                ItemType::ProcDerive => self.derives.insert(ItemEntry::new(new_url, name)),
                ItemType::TraitAlias => self.trait_aliases.insert(ItemEntry::new(new_url, name)),
                _ => true,
            };
        }
    }
}

impl AllTypes {
    fn print(self, f: &mut Buffer) {
        fn print_entries(f: &mut Buffer, e: &FxHashSet<ItemEntry>, title: &str, class: &str) {
            if !e.is_empty() {
                let mut e: Vec<&ItemEntry> = e.iter().collect();
                e.sort();
                write!(f, "<h3 id=\"{}\">{}</h3><ul class=\"{} docblock\">", title, title, class);

                for s in e.iter() {
                    write!(f, "<li>{}</li>", s.print());
                }

                f.write_str("</ul>");
            }
        }

        f.write_str(
            "<h1 class=\"fqn\">\
                 <span class=\"in-band\">List of all items</span>\
                 <span class=\"out-of-band\">\
                     <span id=\"render-detail\">\
                         <a id=\"toggle-all-docs\" href=\"javascript:void(0)\" \
                            title=\"collapse all docs\">\
                             [<span class=\"inner\">&#x2212;</span>]\
                         </a>\
                     </span>
                 </span>
             </h1>",
        );
        // Note: print_entries does not escape the title, because we know the current set of titles
        // don't require escaping.
        print_entries(f, &self.structs, "Structs", "structs");
        print_entries(f, &self.enums, "Enums", "enums");
        print_entries(f, &self.unions, "Unions", "unions");
        print_entries(f, &self.primitives, "Primitives", "primitives");
        print_entries(f, &self.traits, "Traits", "traits");
        print_entries(f, &self.macros, "Macros", "macros");
        print_entries(f, &self.attributes, "Attribute Macros", "attributes");
        print_entries(f, &self.derives, "Derive Macros", "derives");
        print_entries(f, &self.functions, "Functions", "functions");
        print_entries(f, &self.typedefs, "Typedefs", "typedefs");
        print_entries(f, &self.trait_aliases, "Trait Aliases", "trait-aliases");
        print_entries(f, &self.opaque_tys, "Opaque Types", "opaque-types");
        print_entries(f, &self.statics, "Statics", "statics");
        print_entries(f, &self.constants, "Constants", "constants")
    }
}

#[derive(Debug)]
enum Setting {
    Section {
        description: &'static str,
        sub_settings: Vec<Setting>,
    },
    Toggle {
        js_data_name: &'static str,
        description: &'static str,
        default_value: bool,
    },
    Select {
        js_data_name: &'static str,
        description: &'static str,
        default_value: &'static str,
        options: Vec<(String, String)>,
    },
}

impl Setting {
    fn display(&self, root_path: &str, suffix: &str) -> String {
        match *self {
            Setting::Section { description, ref sub_settings } => format!(
                "<div class=\"setting-line\">\
                     <div class=\"title\">{}</div>\
                     <div class=\"sub-settings\">{}</div>
                 </div>",
                description,
                sub_settings.iter().map(|s| s.display(root_path, suffix)).collect::<String>()
            ),
            Setting::Toggle { js_data_name, description, default_value } => format!(
                "<div class=\"setting-line\">\
                     <label class=\"toggle\">\
                     <input type=\"checkbox\" id=\"{}\" {}>\
                     <span class=\"slider\"></span>\
                     </label>\
                     <div>{}</div>\
                 </div>",
                js_data_name,
                if default_value { " checked" } else { "" },
                description,
            ),
            Setting::Select { js_data_name, description, default_value, ref options } => format!(
                "<div class=\"setting-line\">\
                     <div>{}</div>\
                     <label class=\"select-wrapper\">\
                         <select id=\"{}\" autocomplete=\"off\">{}</select>\
                         <img src=\"{}down-arrow{}.svg\" alt=\"Select item\">\
                     </label>\
                 </div>",
                description,
                js_data_name,
                options
                    .iter()
                    .map(|opt| format!(
                        "<option value=\"{}\" {}>{}</option>",
                        opt.0,
                        if opt.0 == default_value { "selected" } else { "" },
                        opt.1,
                    ))
                    .collect::<String>(),
                root_path,
                suffix,
            ),
        }
    }
}

impl From<(&'static str, &'static str, bool)> for Setting {
    fn from(values: (&'static str, &'static str, bool)) -> Setting {
        Setting::Toggle { js_data_name: values.0, description: values.1, default_value: values.2 }
    }
}

impl<T: Into<Setting>> From<(&'static str, Vec<T>)> for Setting {
    fn from(values: (&'static str, Vec<T>)) -> Setting {
        Setting::Section {
            description: values.0,
            sub_settings: values.1.into_iter().map(|v| v.into()).collect::<Vec<_>>(),
        }
    }
}

fn settings(root_path: &str, suffix: &str, themes: &[StylePath]) -> Result<String, Error> {
    let theme_names: Vec<(String, String)> = themes
        .iter()
        .map(|entry| {
            let theme =
                try_none!(try_none!(entry.path.file_stem(), &entry.path).to_str(), &entry.path)
                    .to_string();

            Ok((theme.clone(), theme))
        })
        .collect::<Result<_, Error>>()?;

    // (id, explanation, default value)
    let settings: &[Setting] = &[
        (
            "Theme preferences",
            vec![
                Setting::from(("use-system-theme", "Use system theme", true)),
                Setting::Select {
                    js_data_name: "preferred-dark-theme",
                    description: "Preferred dark theme",
                    default_value: "dark",
                    options: theme_names.clone(),
                },
                Setting::Select {
                    js_data_name: "preferred-light-theme",
                    description: "Preferred light theme",
                    default_value: "light",
                    options: theme_names,
                },
            ],
        )
            .into(),
        (
            "Auto-hide item declarations",
            vec![
                ("auto-hide-struct", "Auto-hide structs declaration", true),
                ("auto-hide-enum", "Auto-hide enums declaration", false),
                ("auto-hide-union", "Auto-hide unions declaration", true),
                ("auto-hide-trait", "Auto-hide traits declaration", true),
                ("auto-hide-macro", "Auto-hide macros declaration", false),
            ],
        )
            .into(),
        ("auto-hide-attributes", "Auto-hide item attributes.", true).into(),
        ("auto-hide-method-docs", "Auto-hide item methods' documentation", false).into(),
        ("auto-hide-trait-implementations", "Auto-hide trait implementation documentation", true)
            .into(),
        ("auto-collapse-implementors", "Auto-hide implementors of a trait", true).into(),
        ("go-to-only-result", "Directly go to item in search if there is only one result", false)
            .into(),
        ("line-numbers", "Show line numbers on code examples", false).into(),
        ("disable-shortcuts", "Disable keyboard shortcuts", false).into(),
    ];

    Ok(format!(
        "<h1 class=\"fqn\">\
            <span class=\"in-band\">Rustdoc settings</span>\
        </h1>\
        <div class=\"settings\">{}</div>\
        <script src=\"{}settings{}.js\"></script>",
        settings.iter().map(|s| s.display(root_path, suffix)).collect::<String>(),
        root_path,
        suffix
    ))
}

fn document(w: &mut Buffer, cx: &Context<'_>, item: &clean::Item, parent: Option<&clean::Item>) {
    if let Some(ref name) = item.name {
        info!("Documenting {}", name);
    }
    document_item_info(w, cx, item, false, parent);
    document_full(w, item, cx, "", false);
}

/// Render md_text as markdown.
fn render_markdown(
    w: &mut Buffer,
    cx: &Context<'_>,
    md_text: &str,
    links: Vec<RenderedLink>,
    prefix: &str,
    is_hidden: bool,
) {
    let mut ids = cx.id_map.borrow_mut();
    write!(
        w,
        "<div class=\"docblock{}\">{}{}</div>",
        if is_hidden { " hidden" } else { "" },
        prefix,
        Markdown(
            md_text,
            &links,
            &mut ids,
            cx.shared.codes,
            cx.shared.edition,
            &cx.shared.playground
        )
        .into_string()
    )
}

/// Writes a documentation block containing only the first paragraph of the documentation. If the
/// docs are longer, a "Read more" link is appended to the end.
fn document_short(
    w: &mut Buffer,
    item: &clean::Item,
    cx: &Context<'_>,
    link: AssocItemLink<'_>,
    prefix: &str,
    is_hidden: bool,
    parent: Option<&clean::Item>,
    show_def_docs: bool,
) {
    document_item_info(w, cx, item, is_hidden, parent);
    if !show_def_docs {
        return;
    }
    if let Some(s) = item.doc_value() {
        let mut summary_html = MarkdownSummaryLine(&s, &item.links(&cx.cache)).into_string();

        if s.contains('\n') {
            let link =
                format!(r#" <a href="{}">Read more</a>"#, naive_assoc_href(item, link, cx.cache()));

            if let Some(idx) = summary_html.rfind("</p>") {
                summary_html.insert_str(idx, &link);
            } else {
                summary_html.push_str(&link);
            }
        }

        write!(
            w,
            "<div class='docblock{}'>{}{}</div>",
            if is_hidden { " hidden" } else { "" },
            prefix,
            summary_html,
        );
    } else if !prefix.is_empty() {
        write!(
            w,
            "<div class=\"docblock{}\">{}</div>",
            if is_hidden { " hidden" } else { "" },
            prefix
        );
    }
}

fn document_full(
    w: &mut Buffer,
    item: &clean::Item,
    cx: &Context<'_>,
    prefix: &str,
    is_hidden: bool,
) {
    if let Some(s) = cx.shared.maybe_collapsed_doc_value(item) {
        debug!("Doc block: =====\n{}\n=====", s);
        render_markdown(w, cx, &*s, item.links(&cx.cache), prefix, is_hidden);
    } else if !prefix.is_empty() {
        if is_hidden {
            w.write_str("<div class=\"docblock hidden\">");
        } else {
            w.write_str("<div class=\"docblock\">");
        }
        w.write_str(prefix);
        w.write_str("</div>");
    }
}

/// Add extra information about an item such as:
///
/// * Stability
/// * Deprecated
/// * Required features (through the `doc_cfg` feature)
fn document_item_info(
    w: &mut Buffer,
    cx: &Context<'_>,
    item: &clean::Item,
    is_hidden: bool,
    parent: Option<&clean::Item>,
) {
    let item_infos = short_item_info(item, cx, parent);
    if !item_infos.is_empty() {
        if is_hidden {
            w.write_str("<div class=\"item-info hidden\">");
        } else {
            w.write_str("<div class=\"item-info\">");
        }
        for info in item_infos {
            w.write_str(&info);
        }
        w.write_str("</div>");
    }
}

fn portability(item: &clean::Item, parent: Option<&clean::Item>) -> Option<String> {
    let cfg = match (&item.attrs.cfg, parent.and_then(|p| p.attrs.cfg.as_ref())) {
        (Some(cfg), Some(parent_cfg)) => cfg.simplify_with(parent_cfg),
        (cfg, _) => cfg.as_deref().cloned(),
    };

    debug!(
        "Portability {:?} - {:?} = {:?}",
        item.attrs.cfg,
        parent.and_then(|p| p.attrs.cfg.as_ref()),
        cfg
    );

    Some(format!("<div class=\"stab portability\">{}</div>", cfg?.render_long_html()))
}

/// Render the stability, deprecation and portability information that is displayed at the top of
/// the item's documentation.
fn short_item_info(
    item: &clean::Item,
    cx: &Context<'_>,
    parent: Option<&clean::Item>,
) -> Vec<String> {
    let mut extra_info = vec![];
    let error_codes = cx.shared.codes;

    if let Some(Deprecation { note, since, is_since_rustc_version, suggestion: _ }) =
        item.deprecation(cx.tcx())
    {
        // We display deprecation messages for #[deprecated] and #[rustc_deprecated]
        // but only display the future-deprecation messages for #[rustc_deprecated].
        let mut message = if let Some(since) = since {
            let since = &since.as_str();
            if !stability::deprecation_in_effect(is_since_rustc_version, Some(since)) {
                if *since == "TBD" {
                    String::from("Deprecating in a future Rust version")
                } else {
                    format!("Deprecating in {}", Escape(since))
                }
            } else {
                format!("Deprecated since {}", Escape(since))
            }
        } else {
            String::from("Deprecated")
        };

        if let Some(note) = note {
            let note = note.as_str();
            let mut ids = cx.id_map.borrow_mut();
            let html = MarkdownHtml(
                &note,
                &mut ids,
                error_codes,
                cx.shared.edition,
                &cx.shared.playground,
            );
            message.push_str(&format!(": {}", html.into_string()));
        }
        extra_info.push(format!(
            "<div class=\"stab deprecated\"><span class=\"emoji\">👎</span> {}</div>",
            message,
        ));
    }

    // Render unstable items. But don't render "rustc_private" crates (internal compiler crates).
    // Those crates are permanently unstable so it makes no sense to render "unstable" everywhere.
    if let Some((StabilityLevel::Unstable { reason, issue, .. }, feature)) = item
        .stability(cx.tcx())
        .as_ref()
        .filter(|stab| stab.feature != sym::rustc_private)
        .map(|stab| (stab.level, stab.feature))
    {
        let mut message =
            "<span class=\"emoji\">🔬</span> This is a nightly-only experimental API.".to_owned();

        let mut feature = format!("<code>{}</code>", Escape(&feature.as_str()));
        if let (Some(url), Some(issue)) = (&cx.shared.issue_tracker_base_url, issue) {
            feature.push_str(&format!(
                "&nbsp;<a href=\"{url}{issue}\">#{issue}</a>",
                url = url,
                issue = issue
            ));
        }

        message.push_str(&format!(" ({})", feature));

        if let Some(unstable_reason) = reason {
            let mut ids = cx.id_map.borrow_mut();
            message = format!(
                "<details><summary>{}</summary>{}</details>",
                message,
                MarkdownHtml(
                    &unstable_reason.as_str(),
                    &mut ids,
                    error_codes,
                    cx.shared.edition,
                    &cx.shared.playground,
                )
                .into_string()
            );
        }

        extra_info.push(format!("<div class=\"stab unstable\">{}</div>", message));
    }

    if let Some(portability) = portability(item, parent) {
        extra_info.push(portability);
    }

    extra_info
}

fn render_impls(
    cx: &Context<'_>,
    w: &mut Buffer,
    traits: &[&&Impl],
    containing_item: &clean::Item,
) {
    let cache = cx.cache();
    let tcx = cx.tcx();
    let mut impls = traits
        .iter()
        .map(|i| {
            let did = i.trait_did_full(cache).unwrap();
            let assoc_link = AssocItemLink::GotoSource(did, &i.inner_impl().provided_trait_methods);
            let mut buffer = if w.is_for_html() { Buffer::html() } else { Buffer::new() };
            render_impl(
                &mut buffer,
                cx,
                i,
                containing_item,
                assoc_link,
                RenderMode::Normal,
                containing_item.stable_since(tcx).as_deref(),
                containing_item.const_stable_since(tcx).as_deref(),
                true,
                None,
                false,
                true,
                &[],
            );
            buffer.into_inner()
        })
        .collect::<Vec<_>>();
    impls.sort();
    w.write_str(&impls.join(""));
}

fn naive_assoc_href(it: &clean::Item, link: AssocItemLink<'_>, cache: &Cache) -> String {
    use crate::formats::item_type::ItemType::*;

    let name = it.name.as_ref().unwrap();
    let ty = match it.type_() {
        Typedef | AssocType => AssocType,
        s => s,
    };

    let anchor = format!("#{}.{}", ty, name);
    match link {
        AssocItemLink::Anchor(Some(ref id)) => format!("#{}", id),
        AssocItemLink::Anchor(None) => anchor,
        AssocItemLink::GotoSource(did, _) => {
            href(did, cache).map(|p| format!("{}{}", p.0, anchor)).unwrap_or(anchor)
        }
    }
}

fn assoc_const(
    w: &mut Buffer,
    it: &clean::Item,
    ty: &clean::Type,
    _default: Option<&String>,
    link: AssocItemLink<'_>,
    extra: &str,
    cx: &Context<'_>,
) {
    let cache = cx.cache();
    let tcx = cx.tcx();
    write!(
        w,
        "{}{}const <a href=\"{}\" class=\"constant\"><b>{}</b></a>: {}",
        extra,
        it.visibility.print_with_space(tcx, it.def_id, cache),
        naive_assoc_href(it, link, cache),
        it.name.as_ref().unwrap(),
        ty.print(cache, tcx)
    );
}

fn assoc_type(
    w: &mut Buffer,
    it: &clean::Item,
    bounds: &[clean::GenericBound],
    default: Option<&clean::Type>,
    link: AssocItemLink<'_>,
    extra: &str,
    cache: &Cache,
    tcx: TyCtxt<'_>,
) {
    write!(
        w,
        "{}type <a href=\"{}\" class=\"type\">{}</a>",
        extra,
        naive_assoc_href(it, link, cache),
        it.name.as_ref().unwrap()
    );
    if !bounds.is_empty() {
        write!(w, ": {}", print_generic_bounds(bounds, cache, tcx))
    }
    if let Some(default) = default {
        write!(w, " = {}", default.print(cache, tcx))
    }
}

fn render_stability_since_raw(
    w: &mut Buffer,
    ver: Option<&str>,
    const_ver: Option<&str>,
    containing_ver: Option<&str>,
    containing_const_ver: Option<&str>,
) {
    let ver = ver.filter(|inner| !inner.is_empty());
    let const_ver = const_ver.filter(|inner| !inner.is_empty());

    match (ver, const_ver) {
        (Some(v), Some(cv)) if const_ver != containing_const_ver => {
            write!(
                w,
                "<span class=\"since\" title=\"Stable since Rust version {0}, const since {1}\">{0} (const: {1})</span>",
                v, cv
            );
        }
        (Some(v), _) if ver != containing_ver => {
            write!(
                w,
                "<span class=\"since\" title=\"Stable since Rust version {0}\">{0}</span>",
                v
            );
        }
        _ => {}
    }
}

fn render_assoc_item(
    w: &mut Buffer,
    item: &clean::Item,
    link: AssocItemLink<'_>,
    parent: ItemType,
    cx: &Context<'_>,
) {
    fn method(
        w: &mut Buffer,
        meth: &clean::Item,
        header: hir::FnHeader,
        g: &clean::Generics,
        d: &clean::FnDecl,
        link: AssocItemLink<'_>,
        parent: ItemType,
        cx: &Context<'_>,
    ) {
        let cache = cx.cache();
        let tcx = cx.tcx();
        let name = meth.name.as_ref().unwrap();
        let href = match link {
            AssocItemLink::Anchor(Some(ref id)) => format!("#{}", id),
            AssocItemLink::Anchor(None) => format!("#{}.{}", meth.type_(), name),
            AssocItemLink::GotoSource(did, provided_methods) => {
                // We're creating a link from an impl-item to the corresponding
                // trait-item and need to map the anchored type accordingly.
                let ty = if provided_methods.contains(&name) {
                    ItemType::Method
                } else {
                    ItemType::TyMethod
                };

                href(did, cache)
                    .map(|p| format!("{}#{}.{}", p.0, ty, name))
                    .unwrap_or_else(|| format!("#{}.{}", ty, name))
            }
        };
        let vis = meth.visibility.print_with_space(tcx, meth.def_id, cache).to_string();
        let constness = header.constness.print_with_space();
        let asyncness = header.asyncness.print_with_space();
        let unsafety = header.unsafety.print_with_space();
        let defaultness = print_default_space(meth.is_default());
        let abi = print_abi_with_space(header.abi).to_string();
        // NOTE: `{:#}` does not print HTML formatting, `{}` does. So `g.print` can't be reused between the length calculation and `write!`.
        let generics_len = format!("{:#}", g.print(cache, tcx)).len();
        let mut header_len = "fn ".len()
            + vis.len()
            + constness.len()
            + asyncness.len()
            + unsafety.len()
            + defaultness.len()
            + abi.len()
            + name.as_str().len()
            + generics_len;

        let (indent, end_newline) = if parent == ItemType::Trait {
            header_len += 4;
            (4, false)
        } else {
            (0, true)
        };
        render_attributes(w, meth, false);
        w.reserve(header_len + "<a href=\"\" class=\"fnname\">{".len() + "</a>".len());
        write!(
            w,
            "{}{}{}{}{}{}{}fn <a href=\"{href}\" class=\"fnname\">{name}</a>\
             {generics}{decl}{notable_traits}{where_clause}",
            if parent == ItemType::Trait { "    " } else { "" },
            vis,
            constness,
            asyncness,
            unsafety,
            defaultness,
            abi,
            href = href,
            name = name,
            generics = g.print(cache, tcx),
            decl = d.full_print(cache, tcx, header_len, indent, header.asyncness),
            notable_traits = notable_traits_decl(&d, cache, tcx),
            where_clause = print_where_clause(g, cache, tcx, indent, end_newline),
        )
    }
    match *item.kind {
        clean::StrippedItem(..) => {}
        clean::TyMethodItem(ref m) => {
            method(w, item, m.header, &m.generics, &m.decl, link, parent, cx)
        }
        clean::MethodItem(ref m, _) => {
            method(w, item, m.header, &m.generics, &m.decl, link, parent, cx)
        }
        clean::AssocConstItem(ref ty, ref default) => assoc_const(
            w,
            item,
            ty,
            default.as_ref(),
            link,
            if parent == ItemType::Trait { "    " } else { "" },
            cx,
        ),
        clean::AssocTypeItem(ref bounds, ref default) => assoc_type(
            w,
            item,
            bounds,
            default.as_ref(),
            link,
            if parent == ItemType::Trait { "    " } else { "" },
            cx.cache(),
            cx.tcx(),
        ),
        _ => panic!("render_assoc_item called on non-associated-item"),
    }
}

const ALLOWED_ATTRIBUTES: &[Symbol] = &[
    sym::export_name,
    sym::lang,
    sym::link_section,
    sym::must_use,
    sym::no_mangle,
    sym::repr,
    sym::non_exhaustive,
];

// The `top` parameter is used when generating the item declaration to ensure it doesn't have a
// left padding. For example:
//
// #[foo] <----- "top" attribute
// struct Foo {
//     #[bar] <---- not "top" attribute
//     bar: usize,
// }
fn render_attributes(w: &mut Buffer, it: &clean::Item, top: bool) {
    let attrs = it
        .attrs
        .other_attrs
        .iter()
        .filter_map(|attr| {
            if ALLOWED_ATTRIBUTES.contains(&attr.name_or_empty()) {
                Some(pprust::attribute_to_string(&attr))
            } else {
                None
            }
        })
        .join("\n");

    if !attrs.is_empty() {
        write!(
            w,
            "<span class=\"docblock attributes{}\">{}</span>",
            if top { " top-attr" } else { "" },
            &attrs
        );
    }
}

#[derive(Copy, Clone)]
enum AssocItemLink<'a> {
    Anchor(Option<&'a str>),
    GotoSource(DefId, &'a FxHashSet<Symbol>),
}

impl<'a> AssocItemLink<'a> {
    fn anchor(&self, id: &'a str) -> Self {
        match *self {
            AssocItemLink::Anchor(_) => AssocItemLink::Anchor(Some(&id)),
            ref other => *other,
        }
    }
}

fn render_assoc_items(
    w: &mut Buffer,
    cx: &Context<'_>,
    containing_item: &clean::Item,
    it: DefId,
    what: AssocItemRender<'_>,
) {
    info!("Documenting associated items of {:?}", containing_item.name);
    let v = match cx.cache.impls.get(&it) {
        Some(v) => v,
        None => return,
    };
    let tcx = cx.tcx();
    let cache = cx.cache();
    let (non_trait, traits): (Vec<_>, _) = v.iter().partition(|i| i.inner_impl().trait_.is_none());
    if !non_trait.is_empty() {
        let render_mode = match what {
            AssocItemRender::All => {
                w.write_str(
                    "<h2 id=\"implementations\" class=\"small-section-header\">\
                         Implementations<a href=\"#implementations\" class=\"anchor\"></a>\
                    </h2>",
                );
                RenderMode::Normal
            }
            AssocItemRender::DerefFor { trait_, type_, deref_mut_ } => {
                let id = cx.derive_id(small_url_encode(format!(
                    "deref-methods-{:#}",
                    type_.print(cache, tcx)
                )));
                debug!("Adding {} to deref id map", type_.print(cache, tcx));
                cx.deref_id_map.borrow_mut().insert(type_.def_id_full(cache).unwrap(), id.clone());
                write!(
                    w,
                    "<h2 id=\"{id}\" class=\"small-section-header\">\
                         Methods from {trait_}&lt;Target = {type_}&gt;\
                         <a href=\"#{id}\" class=\"anchor\"></a>\
                     </h2>",
                    id = id,
                    trait_ = trait_.print(cache, tcx),
                    type_ = type_.print(cache, tcx),
                );
                RenderMode::ForDeref { mut_: deref_mut_ }
            }
        };
        for i in &non_trait {
            render_impl(
                w,
                cx,
                i,
                containing_item,
                AssocItemLink::Anchor(None),
                render_mode,
                containing_item.stable_since(tcx).as_deref(),
                containing_item.const_stable_since(tcx).as_deref(),
                true,
                None,
                false,
                true,
                &[],
            );
        }
    }
    if !traits.is_empty() {
        let deref_impl = traits
            .iter()
            .find(|t| t.inner_impl().trait_.def_id_full(cache) == cx.cache.deref_trait_did);
        if let Some(impl_) = deref_impl {
            let has_deref_mut = traits
                .iter()
                .any(|t| t.inner_impl().trait_.def_id_full(cache) == cx.cache.deref_mut_trait_did);
            render_deref_methods(w, cx, impl_, containing_item, has_deref_mut);
        }

        // If we were already one level into rendering deref methods, we don't want to render
        // anything after recursing into any further deref methods above.
        if let AssocItemRender::DerefFor { .. } = what {
            return;
        }

        let (synthetic, concrete): (Vec<&&Impl>, Vec<&&Impl>) =
            traits.iter().partition(|t| t.inner_impl().synthetic);
        let (blanket_impl, concrete): (Vec<&&Impl>, _) =
            concrete.into_iter().partition(|t| t.inner_impl().blanket_impl.is_some());

        let mut impls = Buffer::empty_from(&w);
        render_impls(cx, &mut impls, &concrete, containing_item);
        let impls = impls.into_inner();
        if !impls.is_empty() {
            write!(
                w,
                "<h2 id=\"trait-implementations\" class=\"small-section-header\">\
                     Trait Implementations<a href=\"#trait-implementations\" class=\"anchor\"></a>\
                 </h2>\
                 <div id=\"trait-implementations-list\">{}</div>",
                impls
            );
        }

        if !synthetic.is_empty() {
            w.write_str(
                "<h2 id=\"synthetic-implementations\" class=\"small-section-header\">\
                     Auto Trait Implementations\
                     <a href=\"#synthetic-implementations\" class=\"anchor\"></a>\
                 </h2>\
                 <div id=\"synthetic-implementations-list\">",
            );
            render_impls(cx, w, &synthetic, containing_item);
            w.write_str("</div>");
        }

        if !blanket_impl.is_empty() {
            w.write_str(
                "<h2 id=\"blanket-implementations\" class=\"small-section-header\">\
                     Blanket Implementations\
                     <a href=\"#blanket-implementations\" class=\"anchor\"></a>\
                 </h2>\
                 <div id=\"blanket-implementations-list\">",
            );
            render_impls(cx, w, &blanket_impl, containing_item);
            w.write_str("</div>");
        }
    }
}

fn render_deref_methods(
    w: &mut Buffer,
    cx: &Context<'_>,
    impl_: &Impl,
    container_item: &clean::Item,
    deref_mut: bool,
) {
    let deref_type = impl_.inner_impl().trait_.as_ref().unwrap();
    let (target, real_target) = impl_
        .inner_impl()
        .items
        .iter()
        .find_map(|item| match *item.kind {
            clean::TypedefItem(ref t, true) => Some(match *t {
                clean::Typedef { item_type: Some(ref type_), .. } => (type_, &t.type_),
                _ => (&t.type_, &t.type_),
            }),
            _ => None,
        })
        .expect("Expected associated type binding");
    debug!("Render deref methods for {:#?}, target {:#?}", impl_.inner_impl().for_, target);
    let what =
        AssocItemRender::DerefFor { trait_: deref_type, type_: real_target, deref_mut_: deref_mut };
    if let Some(did) = target.def_id_full(cx.cache()) {
        if let Some(type_did) = impl_.inner_impl().for_.def_id_full(cx.cache()) {
            // `impl Deref<Target = S> for S`
            if did == type_did {
                // Avoid infinite cycles
                return;
            }
        }
        render_assoc_items(w, cx, container_item, did, what);
    } else {
        if let Some(prim) = target.primitive_type() {
            if let Some(&did) = cx.cache.primitive_locations.get(&prim) {
                render_assoc_items(w, cx, container_item, did, what);
            }
        }
    }
}

fn should_render_item(item: &clean::Item, deref_mut_: bool, cache: &Cache) -> bool {
    let self_type_opt = match *item.kind {
        clean::MethodItem(ref method, _) => method.decl.self_type(),
        clean::TyMethodItem(ref method) => method.decl.self_type(),
        _ => None,
    };

    if let Some(self_ty) = self_type_opt {
        let (by_mut_ref, by_box, by_value) = match self_ty {
            SelfTy::SelfBorrowed(_, mutability)
            | SelfTy::SelfExplicit(clean::BorrowedRef { mutability, .. }) => {
                (mutability == Mutability::Mut, false, false)
            }
            SelfTy::SelfExplicit(clean::ResolvedPath { did, .. }) => {
                (false, Some(did) == cache.owned_box_did, false)
            }
            SelfTy::SelfValue => (false, false, true),
            _ => (false, false, false),
        };

        (deref_mut_ || !by_mut_ref) && !by_box && !by_value
    } else {
        false
    }
}

fn notable_traits_decl(decl: &clean::FnDecl, cache: &Cache, tcx: TyCtxt<'_>) -> String {
    let mut out = Buffer::html();
    let mut trait_ = String::new();

    if let Some(did) = decl.output.def_id_full(cache) {
        if let Some(impls) = cache.impls.get(&did) {
            for i in impls {
                let impl_ = i.inner_impl();
                if impl_
                    .trait_
                    .def_id()
                    .map_or(false, |d| cache.traits.get(&d).map(|t| t.is_notable).unwrap_or(false))
                {
                    if out.is_empty() {
                        write!(
                            &mut out,
                            "<h3 class=\"notable\">Notable traits for {}</h3>\
                             <code class=\"content\">",
                            impl_.for_.print(cache, tcx)
                        );
                        trait_.push_str(&impl_.for_.print(cache, tcx).to_string());
                    }

                    //use the "where" class here to make it small
                    write!(
                        &mut out,
                        "<span class=\"where fmt-newline\">{}</span>",
                        impl_.print(cache, false, tcx)
                    );
                    let t_did = impl_.trait_.def_id_full(cache).unwrap();
                    for it in &impl_.items {
                        if let clean::TypedefItem(ref tydef, _) = *it.kind {
                            out.push_str("<span class=\"where fmt-newline\">    ");
                            assoc_type(
                                &mut out,
                                it,
                                &[],
                                Some(&tydef.type_),
                                AssocItemLink::GotoSource(t_did, &FxHashSet::default()),
                                "",
                                cache,
                                tcx,
                            );
                            out.push_str(";</span>");
                        }
                    }
                }
            }
        }
    }

    if !out.is_empty() {
        out.insert_str(
            0,
            "<span class=\"notable-traits\"><span class=\"notable-traits-tooltip\">ⓘ\
            <div class=\"notable-traits-tooltiptext\"><span class=\"docblock\">",
        );
        out.push_str("</code></span></div></span></span>");
    }

    out.into_inner()
}

fn render_impl(
    w: &mut Buffer,
    cx: &Context<'_>,
    i: &Impl,
    parent: &clean::Item,
    link: AssocItemLink<'_>,
    render_mode: RenderMode,
    outer_version: Option<&str>,
    outer_const_version: Option<&str>,
    show_def_docs: bool,
    use_absolute: Option<bool>,
    is_on_foreign_type: bool,
    show_default_items: bool,
    // This argument is used to reference same type with different paths to avoid duplication
    // in documentation pages for trait with automatic implementations like "Send" and "Sync".
    aliases: &[String],
) {
    let traits = &cx.cache.traits;
    let tcx = cx.tcx();
    let cache = cx.cache();
    let trait_ = i.trait_did_full(cache).map(|did| &traits[&did]);

    if render_mode == RenderMode::Normal {
        let id = cx.derive_id(match i.inner_impl().trait_ {
            Some(ref t) => {
                if is_on_foreign_type {
                    get_id_for_impl_on_foreign_type(&i.inner_impl().for_, t, cache, tcx)
                } else {
                    format!("impl-{}", small_url_encode(format!("{:#}", t.print(cache, tcx))))
                }
            }
            None => "impl".to_string(),
        });
        let aliases = if aliases.is_empty() {
            String::new()
        } else {
            format!(" aliases=\"{}\"", aliases.join(","))
        };
        if let Some(use_absolute) = use_absolute {
            write!(w, "<h3 id=\"{}\" class=\"impl\"{}><code class=\"in-band\">", id, aliases);
            write!(w, "{}", i.inner_impl().print(cache, use_absolute, tcx));
            if show_def_docs {
                for it in &i.inner_impl().items {
                    if let clean::TypedefItem(ref tydef, _) = *it.kind {
                        w.write_str("<span class=\"where fmt-newline\">  ");
                        assoc_type(
                            w,
                            it,
                            &[],
                            Some(&tydef.type_),
                            AssocItemLink::Anchor(None),
                            "",
                            cache,
                            tcx,
                        );
                        w.write_str(";</span>");
                    }
                }
            }
            w.write_str("</code>");
        } else {
            write!(
                w,
                "<h3 id=\"{}\" class=\"impl\"{}><code class=\"in-band\">{}</code>",
                id,
                aliases,
                i.inner_impl().print(cache, false, tcx)
            );
        }
        write!(w, "<a href=\"#{}\" class=\"anchor\"></a>", id);
        render_stability_since_raw(
            w,
            i.impl_item.stable_since(tcx).as_deref(),
            i.impl_item.const_stable_since(tcx).as_deref(),
            outer_version,
            outer_const_version,
        );
        write_srclink(cx, &i.impl_item, w);
        w.write_str("</h3>");

        if trait_.is_some() {
            if let Some(portability) = portability(&i.impl_item, Some(parent)) {
                write!(w, "<div class=\"item-info\">{}</div>", portability);
            }
        }

        if let Some(ref dox) = cx.shared.maybe_collapsed_doc_value(&i.impl_item) {
            let mut ids = cx.id_map.borrow_mut();
            write!(
                w,
                "<div class=\"docblock\">{}</div>",
                Markdown(
                    &*dox,
                    &i.impl_item.links(&cx.cache),
                    &mut ids,
                    cx.shared.codes,
                    cx.shared.edition,
                    &cx.shared.playground
                )
                .into_string()
            );
        }
    }

    fn doc_impl_item(
        w: &mut Buffer,
        cx: &Context<'_>,
        item: &clean::Item,
        parent: &clean::Item,
        link: AssocItemLink<'_>,
        render_mode: RenderMode,
        is_default_item: bool,
        outer_version: Option<&str>,
        outer_const_version: Option<&str>,
        trait_: Option<&clean::Trait>,
        show_def_docs: bool,
    ) {
        let item_type = item.type_();
        let name = item.name.as_ref().unwrap();
        let tcx = cx.tcx();

        let render_method_item = match render_mode {
            RenderMode::Normal => true,
            RenderMode::ForDeref { mut_: deref_mut_ } => {
                should_render_item(&item, deref_mut_, &cx.cache)
            }
        };

        let (is_hidden, extra_class) =
            if (trait_.is_none() || item.doc_value().is_some() || item.kind.is_type_alias())
                && !is_default_item
            {
                (false, "")
            } else {
                (true, " hidden")
            };
        let in_trait_class = if trait_.is_some() { " trait-impl" } else { "" };
        match *item.kind {
            clean::MethodItem(..) | clean::TyMethodItem(_) => {
                // Only render when the method is not static or we allow static methods
                if render_method_item {
                    let id = cx.derive_id(format!("{}.{}", item_type, name));
                    let source_id = trait_
                        .and_then(|trait_| {
                            trait_.items.iter().find(|item| {
                                item.name.map(|n| n.as_str().eq(&name.as_str())).unwrap_or(false)
                            })
                        })
                        .map(|item| format!("{}.{}", item.type_(), name));
                    write!(
                        w,
                        "<h4 id=\"{}\" class=\"{}{}{}\">",
                        id, item_type, extra_class, in_trait_class,
                    );
                    w.write_str("<code>");
                    render_assoc_item(
                        w,
                        item,
                        link.anchor(source_id.as_ref().unwrap_or(&id)),
                        ItemType::Impl,
                        cx,
                    );
                    w.write_str("</code>");
                    render_stability_since_raw(
                        w,
                        item.stable_since(tcx).as_deref(),
                        item.const_stable_since(tcx).as_deref(),
                        outer_version,
                        outer_const_version,
                    );
                    write!(w, "<a href=\"#{}\" class=\"anchor\"></a>", id);
                    write_srclink(cx, item, w);
                    w.write_str("</h4>");
                }
            }
            clean::TypedefItem(ref tydef, _) => {
                let source_id = format!("{}.{}", ItemType::AssocType, name);
                let id = cx.derive_id(source_id.clone());
                write!(
                    w,
                    "<h4 id=\"{}\" class=\"{}{}{}\"><code>",
                    id, item_type, extra_class, in_trait_class
                );
                assoc_type(
                    w,
                    item,
                    &Vec::new(),
                    Some(&tydef.type_),
                    link.anchor(if trait_.is_some() { &source_id } else { &id }),
                    "",
                    cx.cache(),
                    tcx,
                );
                w.write_str("</code>");
                write!(w, "<a href=\"#{}\" class=\"anchor\"></a>", id);
                w.write_str("</h4>");
            }
            clean::AssocConstItem(ref ty, ref default) => {
                let source_id = format!("{}.{}", item_type, name);
                let id = cx.derive_id(source_id.clone());
                write!(
                    w,
                    "<h4 id=\"{}\" class=\"{}{}{}\"><code>",
                    id, item_type, extra_class, in_trait_class
                );
                assoc_const(
                    w,
                    item,
                    ty,
                    default.as_ref(),
                    link.anchor(if trait_.is_some() { &source_id } else { &id }),
                    "",
                    cx,
                );
                w.write_str("</code>");
                render_stability_since_raw(
                    w,
                    item.stable_since(tcx).as_deref(),
                    item.const_stable_since(tcx).as_deref(),
                    outer_version,
                    outer_const_version,
                );
                write!(w, "<a href=\"#{}\" class=\"anchor\"></a>", id);
                write_srclink(cx, item, w);
                w.write_str("</h4>");
            }
            clean::AssocTypeItem(ref bounds, ref default) => {
                let source_id = format!("{}.{}", item_type, name);
                let id = cx.derive_id(source_id.clone());
                write!(
                    w,
                    "<h4 id=\"{}\" class=\"{}{}{}\"><code>",
                    id, item_type, extra_class, in_trait_class
                );
                assoc_type(
                    w,
                    item,
                    bounds,
                    default.as_ref(),
                    link.anchor(if trait_.is_some() { &source_id } else { &id }),
                    "",
                    cx.cache(),
                    tcx,
                );
                w.write_str("</code>");
                write!(w, "<a href=\"#{}\" class=\"anchor\"></a>", id);
                w.write_str("</h4>");
            }
            clean::StrippedItem(..) => return,
            _ => panic!("can't make docs for trait item with name {:?}", item.name),
        }

        if render_method_item {
            if !is_default_item {
                if let Some(t) = trait_ {
                    // The trait item may have been stripped so we might not
                    // find any documentation or stability for it.
                    if let Some(it) = t.items.iter().find(|i| i.name == item.name) {
                        // We need the stability of the item from the trait
                        // because impls can't have a stability.
                        if item.doc_value().is_some() {
                            document_item_info(w, cx, it, is_hidden, Some(parent));
                            document_full(w, item, cx, "", is_hidden);
                        } else {
                            // In case the item isn't documented,
                            // provide short documentation from the trait.
                            document_short(
                                w,
                                it,
                                cx,
                                link,
                                "",
                                is_hidden,
                                Some(parent),
                                show_def_docs,
                            );
                        }
                    }
                } else {
                    document_item_info(w, cx, item, is_hidden, Some(parent));
                    if show_def_docs {
                        document_full(w, item, cx, "", is_hidden);
                    }
                }
            } else {
                document_short(w, item, cx, link, "", is_hidden, Some(parent), show_def_docs);
            }
        }
    }

    w.write_str("<div class=\"impl-items\">");
    for trait_item in &i.inner_impl().items {
        doc_impl_item(
            w,
            cx,
            trait_item,
            if trait_.is_some() { &i.impl_item } else { parent },
            link,
            render_mode,
            false,
            outer_version,
            outer_const_version,
            trait_.map(|t| &t.trait_),
            show_def_docs,
        );
    }

    fn render_default_items(
        w: &mut Buffer,
        cx: &Context<'_>,
        t: &clean::Trait,
        i: &clean::Impl,
        parent: &clean::Item,
        render_mode: RenderMode,
        outer_version: Option<&str>,
        outer_const_version: Option<&str>,
        show_def_docs: bool,
    ) {
        for trait_item in &t.items {
            let n = trait_item.name;
            if i.items.iter().any(|m| m.name == n) {
                continue;
            }
            let did = i.trait_.as_ref().unwrap().def_id_full(cx.cache()).unwrap();
            let assoc_link = AssocItemLink::GotoSource(did, &i.provided_trait_methods);

            doc_impl_item(
                w,
                cx,
                trait_item,
                parent,
                assoc_link,
                render_mode,
                true,
                outer_version,
                outer_const_version,
                Some(t),
                show_def_docs,
            );
        }
    }

    // If we've implemented a trait, then also emit documentation for all
    // default items which weren't overridden in the implementation block.
    // We don't emit documentation for default items if they appear in the
    // Implementations on Foreign Types or Implementors sections.
    if show_default_items {
        if let Some(t) = trait_ {
            render_default_items(
                w,
                cx,
                &t.trait_,
                &i.inner_impl(),
                &i.impl_item,
                render_mode,
                outer_version,
                outer_const_version,
                show_def_docs,
            );
        }
    }
    w.write_str("</div>");
}

fn print_sidebar(cx: &Context<'_>, it: &clean::Item, buffer: &mut Buffer) {
    let parentlen = cx.current.len() - if it.is_mod() { 1 } else { 0 };

    if it.is_struct()
        || it.is_trait()
        || it.is_primitive()
        || it.is_union()
        || it.is_enum()
        || it.is_mod()
        || it.is_typedef()
    {
        write!(
            buffer,
            "<p class=\"location\">{}{}</p>",
            match *it.kind {
                clean::StructItem(..) => "Struct ",
                clean::TraitItem(..) => "Trait ",
                clean::PrimitiveItem(..) => "Primitive Type ",
                clean::UnionItem(..) => "Union ",
                clean::EnumItem(..) => "Enum ",
                clean::TypedefItem(..) => "Type Definition ",
                clean::ForeignTypeItem => "Foreign Type ",
                clean::ModuleItem(..) =>
                    if it.is_crate() {
                        "Crate "
                    } else {
                        "Module "
                    },
                _ => "",
            },
            it.name.as_ref().unwrap()
        );
    }

    if it.is_crate() {
        if let Some(ref version) = cx.cache.crate_version {
            write!(
                buffer,
                "<div class=\"block version\">\
                     <p>Version {}</p>\
                 </div>",
                Escape(version)
            );
        }
    }

    buffer.write_str("<div class=\"sidebar-elems\">");
    if it.is_crate() {
        write!(
            buffer,
            "<a id=\"all-types\" href=\"all.html\"><p>See all {}'s items</p></a>",
            it.name.as_ref().expect("crates always have a name")
        );
    }
    match *it.kind {
        clean::StructItem(ref s) => sidebar_struct(cx, buffer, it, s),
        clean::TraitItem(ref t) => sidebar_trait(cx, buffer, it, t),
        clean::PrimitiveItem(_) => sidebar_primitive(cx, buffer, it),
        clean::UnionItem(ref u) => sidebar_union(cx, buffer, it, u),
        clean::EnumItem(ref e) => sidebar_enum(cx, buffer, it, e),
        clean::TypedefItem(_, _) => sidebar_typedef(cx, buffer, it),
        clean::ModuleItem(ref m) => sidebar_module(buffer, &m.items),
        clean::ForeignTypeItem => sidebar_foreign_type(cx, buffer, it),
        _ => (),
    }

    // The sidebar is designed to display sibling functions, modules and
    // other miscellaneous information. since there are lots of sibling
    // items (and that causes quadratic growth in large modules),
    // we refactor common parts into a shared JavaScript file per module.
    // still, we don't move everything into JS because we want to preserve
    // as much HTML as possible in order to allow non-JS-enabled browsers
    // to navigate the documentation (though slightly inefficiently).

    buffer.write_str("<p class=\"location\">");
    for (i, name) in cx.current.iter().take(parentlen).enumerate() {
        if i > 0 {
            buffer.write_str("::<wbr>");
        }
        write!(
            buffer,
            "<a href=\"{}index.html\">{}</a>",
            &cx.root_path()[..(cx.current.len() - i - 1) * 3],
            *name
        );
    }
    buffer.write_str("</p>");

    // Sidebar refers to the enclosing module, not this module.
    let relpath = if it.is_mod() { "../" } else { "" };
    write!(
        buffer,
        "<div id=\"sidebar-vars\" data-name=\"{name}\" data-ty=\"{ty}\" data-relpath=\"{path}\">\
        </div>",
        name = it.name.unwrap_or(kw::Empty),
        ty = it.type_(),
        path = relpath
    );
    if parentlen == 0 {
        // There is no sidebar-items.js beyond the crate root path
        // FIXME maybe dynamic crate loading can be merged here
    } else {
        write!(buffer, "<script defer src=\"{path}sidebar-items.js\"></script>", path = relpath);
    }
    // Closes sidebar-elems div.
    buffer.write_str("</div>");
}

fn get_next_url(used_links: &mut FxHashSet<String>, url: String) -> String {
    if used_links.insert(url.clone()) {
        return url;
    }
    let mut add = 1;
    while !used_links.insert(format!("{}-{}", url, add)) {
        add += 1;
    }
    format!("{}-{}", url, add)
}

fn get_methods(
    i: &clean::Impl,
    for_deref: bool,
    used_links: &mut FxHashSet<String>,
    deref_mut: bool,
    cache: &Cache,
) -> Vec<String> {
    i.items
        .iter()
        .filter_map(|item| match item.name {
            Some(ref name) if !name.is_empty() && item.is_method() => {
                if !for_deref || should_render_item(item, deref_mut, cache) {
                    Some(format!(
                        "<a href=\"#{}\">{}</a>",
                        get_next_url(used_links, format!("method.{}", name)),
                        name
                    ))
                } else {
                    None
                }
            }
            _ => None,
        })
        .collect::<Vec<_>>()
}

// The point is to url encode any potential character from a type with genericity.
fn small_url_encode(s: String) -> String {
    let mut st = String::new();
    let mut last_match = 0;
    for (idx, c) in s.char_indices() {
        let escaped = match c {
            '<' => "%3C",
            '>' => "%3E",
            ' ' => "%20",
            '?' => "%3F",
            '\'' => "%27",
            '&' => "%26",
            ',' => "%2C",
            ':' => "%3A",
            ';' => "%3B",
            '[' => "%5B",
            ']' => "%5D",
            '"' => "%22",
            _ => continue,
        };

        st += &s[last_match..idx];
        st += escaped;
        // NOTE: we only expect single byte characters here - which is fine as long as we
        // only match single byte characters
        last_match = idx + 1;
    }

    if last_match != 0 {
        st += &s[last_match..];
        st
    } else {
        s
    }
}

fn sidebar_assoc_items(cx: &Context<'_>, out: &mut Buffer, it: &clean::Item) {
    if let Some(v) = cx.cache.impls.get(&it.def_id) {
        let mut used_links = FxHashSet::default();
        let tcx = cx.tcx();
        let cache = cx.cache();

        {
            let used_links_bor = &mut used_links;
            let mut ret = v
                .iter()
                .filter(|i| i.inner_impl().trait_.is_none())
                .flat_map(move |i| {
                    get_methods(i.inner_impl(), false, used_links_bor, false, &cx.cache)
                })
                .collect::<Vec<_>>();
            if !ret.is_empty() {
                // We want links' order to be reproducible so we don't use unstable sort.
                ret.sort();

                out.push_str(
                    "<a class=\"sidebar-title\" href=\"#implementations\">Methods</a>\
                     <div class=\"sidebar-links\">",
                );
                for line in ret {
                    out.push_str(&line);
                }
                out.push_str("</div>");
            }
        }

        if v.iter().any(|i| i.inner_impl().trait_.is_some()) {
            let format_impls = |impls: Vec<&Impl>| {
                let mut links = FxHashSet::default();

                let mut ret = impls
                    .iter()
                    .filter_map(|it| {
                        if let Some(ref i) = it.inner_impl().trait_ {
                            let i_display = format!("{:#}", i.print(cache, tcx));
                            let out = Escape(&i_display);
                            let encoded = small_url_encode(format!("{:#}", i.print(cache, tcx)));
                            let generated = format!(
                                "<a href=\"#impl-{}\">{}{}</a>",
                                encoded,
                                if it.inner_impl().negative_polarity { "!" } else { "" },
                                out
                            );
                            if links.insert(generated.clone()) { Some(generated) } else { None }
                        } else {
                            None
                        }
                    })
                    .collect::<Vec<String>>();
                ret.sort();
                ret
            };

            let write_sidebar_links = |out: &mut Buffer, links: Vec<String>| {
                out.push_str("<div class=\"sidebar-links\">");
                for link in links {
                    out.push_str(&link);
                }
                out.push_str("</div>");
            };

            let (synthetic, concrete): (Vec<&Impl>, Vec<&Impl>) =
                v.iter().partition::<Vec<_>, _>(|i| i.inner_impl().synthetic);
            let (blanket_impl, concrete): (Vec<&Impl>, Vec<&Impl>) = concrete
                .into_iter()
                .partition::<Vec<_>, _>(|i| i.inner_impl().blanket_impl.is_some());

            let concrete_format = format_impls(concrete);
            let synthetic_format = format_impls(synthetic);
            let blanket_format = format_impls(blanket_impl);

            if !concrete_format.is_empty() {
                out.push_str(
                    "<a class=\"sidebar-title\" href=\"#trait-implementations\">\
                        Trait Implementations</a>",
                );
                write_sidebar_links(out, concrete_format);
            }

            if !synthetic_format.is_empty() {
                out.push_str(
                    "<a class=\"sidebar-title\" href=\"#synthetic-implementations\">\
                        Auto Trait Implementations</a>",
                );
                write_sidebar_links(out, synthetic_format);
            }

            if !blanket_format.is_empty() {
                out.push_str(
                    "<a class=\"sidebar-title\" href=\"#blanket-implementations\">\
                        Blanket Implementations</a>",
                );
                write_sidebar_links(out, blanket_format);
            }

            if let Some(impl_) = v
                .iter()
                .filter(|i| i.inner_impl().trait_.is_some())
                .find(|i| i.inner_impl().trait_.def_id_full(cache) == cx.cache.deref_trait_did)
            {
                sidebar_deref_methods(cx, out, impl_, v);
            }
        }
    }
}

fn sidebar_deref_methods(cx: &Context<'_>, out: &mut Buffer, impl_: &Impl, v: &Vec<Impl>) {
    let c = cx.cache();
    let tcx = cx.tcx();

    debug!("found Deref: {:?}", impl_);
    if let Some((target, real_target)) =
        impl_.inner_impl().items.iter().find_map(|item| match *item.kind {
            clean::TypedefItem(ref t, true) => Some(match *t {
                clean::Typedef { item_type: Some(ref type_), .. } => (type_, &t.type_),
                _ => (&t.type_, &t.type_),
            }),
            _ => None,
        })
    {
        debug!("found target, real_target: {:?} {:?}", target, real_target);
        if let Some(did) = target.def_id_full(c) {
            if let Some(type_did) = impl_.inner_impl().for_.def_id_full(c) {
                // `impl Deref<Target = S> for S`
                if did == type_did {
                    // Avoid infinite cycles
                    return;
                }
            }
        }
        let deref_mut = v
            .iter()
            .filter(|i| i.inner_impl().trait_.is_some())
            .any(|i| i.inner_impl().trait_.def_id_full(c) == c.deref_mut_trait_did);
        let inner_impl = target
            .def_id_full(c)
            .or_else(|| {
                target.primitive_type().and_then(|prim| c.primitive_locations.get(&prim).cloned())
            })
            .and_then(|did| c.impls.get(&did));
        if let Some(impls) = inner_impl {
            debug!("found inner_impl: {:?}", impls);
            let mut used_links = FxHashSet::default();
            let mut ret = impls
                .iter()
                .filter(|i| i.inner_impl().trait_.is_none())
                .flat_map(|i| get_methods(i.inner_impl(), true, &mut used_links, deref_mut, c))
                .collect::<Vec<_>>();
            if !ret.is_empty() {
                let deref_id_map = cx.deref_id_map.borrow();
                let id = deref_id_map
                    .get(&real_target.def_id_full(c).unwrap())
                    .expect("Deref section without derived id");
                write!(
                    out,
                    "<a class=\"sidebar-title\" href=\"#{}\">Methods from {}&lt;Target={}&gt;</a>",
                    id,
                    Escape(&format!(
                        "{:#}",
                        impl_.inner_impl().trait_.as_ref().unwrap().print(c, tcx)
                    )),
                    Escape(&format!("{:#}", real_target.print(c, tcx))),
                );
                // We want links' order to be reproducible so we don't use unstable sort.
                ret.sort();
                out.push_str("<div class=\"sidebar-links\">");
                for link in ret {
                    out.push_str(&link);
                }
                out.push_str("</div>");
            }
        }

        // Recurse into any further impls that might exist for `target`
        if let Some(target_did) = target.def_id_full(c) {
            if let Some(target_impls) = c.impls.get(&target_did) {
                if let Some(target_deref_impl) = target_impls
                    .iter()
                    .filter(|i| i.inner_impl().trait_.is_some())
                    .find(|i| i.inner_impl().trait_.def_id_full(c) == c.deref_trait_did)
                {
                    sidebar_deref_methods(cx, out, target_deref_impl, target_impls);
                }
            }
        }
    }
}

fn sidebar_struct(cx: &Context<'_>, buf: &mut Buffer, it: &clean::Item, s: &clean::Struct) {
    let mut sidebar = Buffer::new();
    let fields = get_struct_fields_name(&s.fields);

    if !fields.is_empty() {
        if let CtorKind::Fictive = s.struct_type {
            sidebar.push_str(
                "<a class=\"sidebar-title\" href=\"#fields\">Fields</a>\
                <div class=\"sidebar-links\">",
            );

            for field in fields {
                sidebar.push_str(&field);
            }

            sidebar.push_str("</div>");
        }
    }

    sidebar_assoc_items(cx, &mut sidebar, it);

    if !sidebar.is_empty() {
        write!(buf, "<div class=\"block items\">{}</div>", sidebar.into_inner());
    }
}

fn get_id_for_impl_on_foreign_type(
    for_: &clean::Type,
    trait_: &clean::Type,
    cache: &Cache,
    tcx: TyCtxt<'_>,
) -> String {
    small_url_encode(format!(
        "impl-{:#}-for-{:#}",
        trait_.print(cache, tcx),
        for_.print(cache, tcx)
    ))
}

fn extract_for_impl_name(
    item: &clean::Item,
    cache: &Cache,
    tcx: TyCtxt<'_>,
) -> Option<(String, String)> {
    match *item.kind {
        clean::ItemKind::ImplItem(ref i) => {
            if let Some(ref trait_) = i.trait_ {
                Some((
                    format!("{:#}", i.for_.print(cache, tcx)),
                    get_id_for_impl_on_foreign_type(&i.for_, trait_, cache, tcx),
                ))
            } else {
                None
            }
        }
        _ => None,
    }
}

fn sidebar_trait(cx: &Context<'_>, buf: &mut Buffer, it: &clean::Item, t: &clean::Trait) {
    buf.write_str("<div class=\"block items\">");

    fn print_sidebar_section(
        out: &mut Buffer,
        items: &[clean::Item],
        before: &str,
        filter: impl Fn(&clean::Item) -> bool,
        write: impl Fn(&mut Buffer, &str),
        after: &str,
    ) {
        let mut items = items
            .iter()
            .filter_map(|m| match m.name {
                Some(ref name) if filter(m) => Some(name.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>();

        if !items.is_empty() {
            items.sort_unstable();
            out.push_str(before);
            for item in items.into_iter() {
                write(out, &item);
            }
            out.push_str(after);
        }
    }

    print_sidebar_section(
        buf,
        &t.items,
        "<a class=\"sidebar-title\" href=\"#associated-types\">\
            Associated Types</a><div class=\"sidebar-links\">",
        |m| m.is_associated_type(),
        |out, sym| write!(out, "<a href=\"#associatedtype.{0}\">{0}</a>", sym),
        "</div>",
    );

    print_sidebar_section(
        buf,
        &t.items,
        "<a class=\"sidebar-title\" href=\"#associated-const\">\
            Associated Constants</a><div class=\"sidebar-links\">",
        |m| m.is_associated_const(),
        |out, sym| write!(out, "<a href=\"#associatedconstant.{0}\">{0}</a>", sym),
        "</div>",
    );

    print_sidebar_section(
        buf,
        &t.items,
        "<a class=\"sidebar-title\" href=\"#required-methods\">\
            Required Methods</a><div class=\"sidebar-links\">",
        |m| m.is_ty_method(),
        |out, sym| write!(out, "<a href=\"#tymethod.{0}\">{0}</a>", sym),
        "</div>",
    );

    print_sidebar_section(
        buf,
        &t.items,
        "<a class=\"sidebar-title\" href=\"#provided-methods\">\
            Provided Methods</a><div class=\"sidebar-links\">",
        |m| m.is_method(),
        |out, sym| write!(out, "<a href=\"#method.{0}\">{0}</a>", sym),
        "</div>",
    );

    if let Some(implementors) = cx.cache.implementors.get(&it.def_id) {
        let cache = cx.cache();
        let tcx = cx.tcx();
        let mut res = implementors
            .iter()
            .filter(|i| {
                i.inner_impl()
                    .for_
                    .def_id_full(cache)
                    .map_or(false, |d| !cx.cache.paths.contains_key(&d))
            })
            .filter_map(|i| extract_for_impl_name(&i.impl_item, cache, tcx))
            .collect::<Vec<_>>();

        if !res.is_empty() {
            res.sort();
            buf.push_str(
                "<a class=\"sidebar-title\" href=\"#foreign-impls\">\
                    Implementations on Foreign Types</a>\
                 <div class=\"sidebar-links\">",
            );
            for (name, id) in res.into_iter() {
                write!(buf, "<a href=\"#{}\">{}</a>", id, Escape(&name));
            }
            buf.push_str("</div>");
        }
    }

    sidebar_assoc_items(cx, buf, it);

    buf.push_str("<a class=\"sidebar-title\" href=\"#implementors\">Implementors</a>");
    if t.is_auto {
        buf.push_str(
            "<a class=\"sidebar-title\" \
                href=\"#synthetic-implementors\">Auto Implementors</a>",
        );
    }

    buf.push_str("</div>")
}

fn sidebar_primitive(cx: &Context<'_>, buf: &mut Buffer, it: &clean::Item) {
    let mut sidebar = Buffer::new();
    sidebar_assoc_items(cx, &mut sidebar, it);

    if !sidebar.is_empty() {
        write!(buf, "<div class=\"block items\">{}</div>", sidebar.into_inner());
    }
}

fn sidebar_typedef(cx: &Context<'_>, buf: &mut Buffer, it: &clean::Item) {
    let mut sidebar = Buffer::new();
    sidebar_assoc_items(cx, &mut sidebar, it);

    if !sidebar.is_empty() {
        write!(buf, "<div class=\"block items\">{}</div>", sidebar.into_inner());
    }
}

fn get_struct_fields_name(fields: &[clean::Item]) -> Vec<String> {
    let mut fields = fields
        .iter()
        .filter(|f| matches!(*f.kind, clean::StructFieldItem(..)))
        .filter_map(|f| {
            f.name.map(|name| format!("<a href=\"#structfield.{name}\">{name}</a>", name = name))
        })
        .collect::<Vec<_>>();
    fields.sort();
    fields
}

fn sidebar_union(cx: &Context<'_>, buf: &mut Buffer, it: &clean::Item, u: &clean::Union) {
    let mut sidebar = Buffer::new();
    let fields = get_struct_fields_name(&u.fields);

    if !fields.is_empty() {
        sidebar.push_str(
            "<a class=\"sidebar-title\" href=\"#fields\">Fields</a>\
            <div class=\"sidebar-links\">",
        );

        for field in fields {
            sidebar.push_str(&field);
        }

        sidebar.push_str("</div>");
    }

    sidebar_assoc_items(cx, &mut sidebar, it);

    if !sidebar.is_empty() {
        write!(buf, "<div class=\"block items\">{}</div>", sidebar.into_inner());
    }
}

fn sidebar_enum(cx: &Context<'_>, buf: &mut Buffer, it: &clean::Item, e: &clean::Enum) {
    let mut sidebar = Buffer::new();

    let mut variants = e
        .variants
        .iter()
        .filter_map(|v| match v.name {
            Some(ref name) => Some(format!("<a href=\"#variant.{name}\">{name}</a>", name = name)),
            _ => None,
        })
        .collect::<Vec<_>>();
    if !variants.is_empty() {
        variants.sort_unstable();
        sidebar.push_str(&format!(
            "<a class=\"sidebar-title\" href=\"#variants\">Variants</a>\
             <div class=\"sidebar-links\">{}</div>",
            variants.join(""),
        ));
    }

    sidebar_assoc_items(cx, &mut sidebar, it);

    if !sidebar.is_empty() {
        write!(buf, "<div class=\"block items\">{}</div>", sidebar.into_inner());
    }
}

fn item_ty_to_strs(ty: &ItemType) -> (&'static str, &'static str) {
    match *ty {
        ItemType::ExternCrate | ItemType::Import => ("reexports", "Re-exports"),
        ItemType::Module => ("modules", "Modules"),
        ItemType::Struct => ("structs", "Structs"),
        ItemType::Union => ("unions", "Unions"),
        ItemType::Enum => ("enums", "Enums"),
        ItemType::Function => ("functions", "Functions"),
        ItemType::Typedef => ("types", "Type Definitions"),
        ItemType::Static => ("statics", "Statics"),
        ItemType::Constant => ("constants", "Constants"),
        ItemType::Trait => ("traits", "Traits"),
        ItemType::Impl => ("impls", "Implementations"),
        ItemType::TyMethod => ("tymethods", "Type Methods"),
        ItemType::Method => ("methods", "Methods"),
        ItemType::StructField => ("fields", "Struct Fields"),
        ItemType::Variant => ("variants", "Variants"),
        ItemType::Macro => ("macros", "Macros"),
        ItemType::Primitive => ("primitives", "Primitive Types"),
        ItemType::AssocType => ("associated-types", "Associated Types"),
        ItemType::AssocConst => ("associated-consts", "Associated Constants"),
        ItemType::ForeignType => ("foreign-types", "Foreign Types"),
        ItemType::Keyword => ("keywords", "Keywords"),
        ItemType::OpaqueTy => ("opaque-types", "Opaque Types"),
        ItemType::ProcAttribute => ("attributes", "Attribute Macros"),
        ItemType::ProcDerive => ("derives", "Derive Macros"),
        ItemType::TraitAlias => ("trait-aliases", "Trait aliases"),
    }
}

fn sidebar_module(buf: &mut Buffer, items: &[clean::Item]) {
    let mut sidebar = String::new();

    if items.iter().any(|it| {
        it.type_() == ItemType::ExternCrate || (it.type_() == ItemType::Import && !it.is_stripped())
    }) {
        sidebar.push_str("<li><a href=\"#reexports\">Re-exports</a></li>");
    }

    // ordering taken from item_module, reorder, where it prioritized elements in a certain order
    // to print its headings
    for &myty in &[
        ItemType::Primitive,
        ItemType::Module,
        ItemType::Macro,
        ItemType::Struct,
        ItemType::Enum,
        ItemType::Constant,
        ItemType::Static,
        ItemType::Trait,
        ItemType::Function,
        ItemType::Typedef,
        ItemType::Union,
        ItemType::Impl,
        ItemType::TyMethod,
        ItemType::Method,
        ItemType::StructField,
        ItemType::Variant,
        ItemType::AssocType,
        ItemType::AssocConst,
        ItemType::ForeignType,
        ItemType::Keyword,
    ] {
        if items.iter().any(|it| !it.is_stripped() && it.type_() == myty) {
            let (short, name) = item_ty_to_strs(&myty);
            sidebar.push_str(&format!(
                "<li><a href=\"#{id}\">{name}</a></li>",
                id = short,
                name = name
            ));
        }
    }

    if !sidebar.is_empty() {
        write!(buf, "<div class=\"block items\"><ul>{}</ul></div>", sidebar);
    }
}

fn sidebar_foreign_type(cx: &Context<'_>, buf: &mut Buffer, it: &clean::Item) {
    let mut sidebar = Buffer::new();
    sidebar_assoc_items(cx, &mut sidebar, it);

    if !sidebar.is_empty() {
        write!(buf, "<div class=\"block items\">{}</div>", sidebar.into_inner());
    }
}

crate const BASIC_KEYWORDS: &str = "rust, rustlang, rust-lang";

/// Returns a list of all paths used in the type.
/// This is used to help deduplicate imported impls
/// for reexported types. If any of the contained
/// types are re-exported, we don't use the corresponding
/// entry from the js file, as inlining will have already
/// picked up the impl
fn collect_paths_for_type(first_ty: clean::Type, cache: &Cache) -> Vec<String> {
    let mut out = Vec::new();
    let mut visited = FxHashSet::default();
    let mut work = VecDeque::new();

    work.push_back(first_ty);

    while let Some(ty) = work.pop_front() {
        if !visited.insert(ty.clone()) {
            continue;
        }

        match ty {
            clean::Type::ResolvedPath { did, .. } => {
                let get_extern = || cache.external_paths.get(&did).map(|s| s.0.clone());
                let fqp = cache.exact_paths.get(&did).cloned().or_else(get_extern);

                if let Some(path) = fqp {
                    out.push(path.join("::"));
                }
            }
            clean::Type::Tuple(tys) => {
                work.extend(tys.into_iter());
            }
            clean::Type::Slice(ty) => {
                work.push_back(*ty);
            }
            clean::Type::Array(ty, _) => {
                work.push_back(*ty);
            }
            clean::Type::RawPointer(_, ty) => {
                work.push_back(*ty);
            }
            clean::Type::BorrowedRef { type_, .. } => {
                work.push_back(*type_);
            }
            clean::Type::QPath { self_type, trait_, .. } => {
                work.push_back(*self_type);
                work.push_back(*trait_);
            }
            _ => {}
        }
    }
    out
}
