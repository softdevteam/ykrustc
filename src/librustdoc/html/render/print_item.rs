use std::cmp::Ordering;

use rustc_data_structures::fx::FxHashMap;
use rustc_hir as hir;
use rustc_hir::def::CtorKind;
use rustc_hir::def_id::DefId;
use rustc_middle::middle::stability;
use rustc_middle::ty::TyCtxt;
use rustc_span::hygiene::MacroKind;
use rustc_span::symbol::{kw, sym, Symbol};

use super::{
    collect_paths_for_type, document, ensure_trailing_slash, item_ty_to_strs, notable_traits_decl,
    render_assoc_item, render_assoc_items, render_attributes, render_impl,
    render_stability_since_raw, write_srclink, AssocItemLink, Context,
};
use crate::clean::{self, GetDefId};
use crate::formats::cache::Cache;
use crate::formats::item_type::ItemType;
use crate::formats::{AssocItemRender, FormatRenderer, Impl, RenderMode};
use crate::html::escape::Escape;
use crate::html::format::{print_abi_with_space, print_where_clause, Buffer, PrintWithSpace};
use crate::html::highlight;
use crate::html::markdown::MarkdownSummaryLine;

pub(super) fn print_item(cx: &Context<'_>, item: &clean::Item, buf: &mut Buffer) {
    debug_assert!(!item.is_stripped());
    // Write the breadcrumb trail header for the top
    buf.write_str("<h1 class=\"fqn\"><span class=\"in-band\">");
    let name = match *item.kind {
        clean::ModuleItem(ref m) => {
            if m.is_crate {
                "Crate "
            } else {
                "Module "
            }
        }
        clean::FunctionItem(..) | clean::ForeignFunctionItem(..) => "Function ",
        clean::TraitItem(..) => "Trait ",
        clean::StructItem(..) => "Struct ",
        clean::UnionItem(..) => "Union ",
        clean::EnumItem(..) => "Enum ",
        clean::TypedefItem(..) => "Type Definition ",
        clean::MacroItem(..) => "Macro ",
        clean::ProcMacroItem(ref mac) => match mac.kind {
            MacroKind::Bang => "Macro ",
            MacroKind::Attr => "Attribute Macro ",
            MacroKind::Derive => "Derive Macro ",
        },
        clean::PrimitiveItem(..) => "Primitive Type ",
        clean::StaticItem(..) | clean::ForeignStaticItem(..) => "Static ",
        clean::ConstantItem(..) => "Constant ",
        clean::ForeignTypeItem => "Foreign Type ",
        clean::KeywordItem(..) => "Keyword ",
        clean::OpaqueTyItem(..) => "Opaque Type ",
        clean::TraitAliasItem(..) => "Trait Alias ",
        _ => {
            // We don't generate pages for any other type.
            unreachable!();
        }
    };
    buf.write_str(name);
    if !item.is_primitive() && !item.is_keyword() {
        let cur = &cx.current;
        let amt = if item.is_mod() { cur.len() - 1 } else { cur.len() };
        for (i, component) in cur.iter().enumerate().take(amt) {
            write!(
                buf,
                "<a href=\"{}index.html\">{}</a>::<wbr>",
                "../".repeat(cur.len() - i - 1),
                component
            );
        }
    }
    write!(buf, "<a class=\"{}\" href=\"\">{}</a>", item.type_(), item.name.as_ref().unwrap());
    write!(buf, "<button id=\"copy-path\" onclick=\"copy_path(this)\">⎘</button>");

    buf.write_str("</span>"); // in-band
    buf.write_str("<span class=\"out-of-band\">");
    render_stability_since_raw(
        buf,
        item.stable_since(cx.tcx()).as_deref(),
        item.const_stable_since(cx.tcx()).as_deref(),
        None,
        None,
    );
    buf.write_str(
        "<span id=\"render-detail\">\
                <a id=\"toggle-all-docs\" href=\"javascript:void(0)\" \
                    title=\"collapse all docs\">\
                    [<span class=\"inner\">&#x2212;</span>]\
                </a>\
            </span>",
    );

    // Write `src` tag
    //
    // When this item is part of a `crate use` in a downstream crate, the
    // [src] link in the downstream documentation will actually come back to
    // this page, and this link will be auto-clicked. The `id` attribute is
    // used to find the link to auto-click.
    if cx.shared.include_sources && !item.is_primitive() {
        write_srclink(cx, item, buf);
    }

    buf.write_str("</span></h1>"); // out-of-band

    match *item.kind {
        clean::ModuleItem(ref m) => item_module(buf, cx, item, &m.items),
        clean::FunctionItem(ref f) | clean::ForeignFunctionItem(ref f) => {
            item_function(buf, cx, item, f)
        }
        clean::TraitItem(ref t) => item_trait(buf, cx, item, t),
        clean::StructItem(ref s) => item_struct(buf, cx, item, s),
        clean::UnionItem(ref s) => item_union(buf, cx, item, s),
        clean::EnumItem(ref e) => item_enum(buf, cx, item, e),
        clean::TypedefItem(ref t, _) => item_typedef(buf, cx, item, t),
        clean::MacroItem(ref m) => item_macro(buf, cx, item, m),
        clean::ProcMacroItem(ref m) => item_proc_macro(buf, cx, item, m),
        clean::PrimitiveItem(_) => item_primitive(buf, cx, item),
        clean::StaticItem(ref i) | clean::ForeignStaticItem(ref i) => item_static(buf, cx, item, i),
        clean::ConstantItem(ref c) => item_constant(buf, cx, item, c),
        clean::ForeignTypeItem => item_foreign_type(buf, cx, item),
        clean::KeywordItem(_) => item_keyword(buf, cx, item),
        clean::OpaqueTyItem(ref e) => item_opaque_ty(buf, cx, item, e),
        clean::TraitAliasItem(ref ta) => item_trait_alias(buf, cx, item, ta),
        _ => {
            // We don't generate pages for any other type.
            unreachable!();
        }
    }
}

fn item_module(w: &mut Buffer, cx: &Context<'_>, item: &clean::Item, items: &[clean::Item]) {
    document(w, cx, item, None);

    let mut indices = (0..items.len()).filter(|i| !items[*i].is_stripped()).collect::<Vec<usize>>();

    // the order of item types in the listing
    fn reorder(ty: ItemType) -> u8 {
        match ty {
            ItemType::ExternCrate => 0,
            ItemType::Import => 1,
            ItemType::Primitive => 2,
            ItemType::Module => 3,
            ItemType::Macro => 4,
            ItemType::Struct => 5,
            ItemType::Enum => 6,
            ItemType::Constant => 7,
            ItemType::Static => 8,
            ItemType::Trait => 9,
            ItemType::Function => 10,
            ItemType::Typedef => 12,
            ItemType::Union => 13,
            _ => 14 + ty as u8,
        }
    }

    fn cmp(
        i1: &clean::Item,
        i2: &clean::Item,
        idx1: usize,
        idx2: usize,
        tcx: TyCtxt<'_>,
    ) -> Ordering {
        let ty1 = i1.type_();
        let ty2 = i2.type_();
        if ty1 != ty2 {
            return (reorder(ty1), idx1).cmp(&(reorder(ty2), idx2));
        }
        let s1 = i1.stability(tcx).as_ref().map(|s| s.level);
        let s2 = i2.stability(tcx).as_ref().map(|s| s.level);
        if let (Some(a), Some(b)) = (s1, s2) {
            match (a.is_stable(), b.is_stable()) {
                (true, true) | (false, false) => {}
                (false, true) => return Ordering::Less,
                (true, false) => return Ordering::Greater,
            }
        }
        let lhs = i1.name.unwrap_or(kw::Empty).as_str();
        let rhs = i2.name.unwrap_or(kw::Empty).as_str();
        compare_names(&lhs, &rhs)
    }

    if cx.shared.sort_modules_alphabetically {
        indices.sort_by(|&i1, &i2| cmp(&items[i1], &items[i2], i1, i2, cx.tcx()));
    }
    // This call is to remove re-export duplicates in cases such as:
    //
    // ```
    // crate mod foo {
    //     crate mod bar {
    //         crate trait Double { fn foo(); }
    //     }
    // }
    //
    // crate use foo::bar::*;
    // crate use foo::*;
    // ```
    //
    // `Double` will appear twice in the generated docs.
    //
    // FIXME: This code is quite ugly and could be improved. Small issue: DefId
    // can be identical even if the elements are different (mostly in imports).
    // So in case this is an import, we keep everything by adding a "unique id"
    // (which is the position in the vector).
    indices.dedup_by_key(|i| {
        (
            items[*i].def_id,
            if items[*i].name.as_ref().is_some() { Some(full_path(cx, &items[*i])) } else { None },
            items[*i].type_(),
            if items[*i].is_import() { *i } else { 0 },
        )
    });

    debug!("{:?}", indices);
    let mut curty = None;
    for &idx in &indices {
        let myitem = &items[idx];
        if myitem.is_stripped() {
            continue;
        }

        let myty = Some(myitem.type_());
        if curty == Some(ItemType::ExternCrate) && myty == Some(ItemType::Import) {
            // Put `extern crate` and `use` re-exports in the same section.
            curty = myty;
        } else if myty != curty {
            if curty.is_some() {
                w.write_str("</table>");
            }
            curty = myty;
            let (short, name) = item_ty_to_strs(&myty.unwrap());
            write!(
                w,
                "<h2 id=\"{id}\" class=\"section-header\">\
                       <a href=\"#{id}\">{name}</a></h2>\n<table>",
                id = cx.derive_id(short.to_owned()),
                name = name
            );
        }

        match *myitem.kind {
            clean::ExternCrateItem { ref src } => {
                use crate::html::format::anchor;

                match *src {
                    Some(ref src) => write!(
                        w,
                        "<tr><td><code>{}extern crate {} as {};",
                        myitem.visibility.print_with_space(cx.tcx(), myitem.def_id, cx.cache()),
                        anchor(myitem.def_id, &*src.as_str(), cx.cache()),
                        myitem.name.as_ref().unwrap(),
                    ),
                    None => write!(
                        w,
                        "<tr><td><code>{}extern crate {};",
                        myitem.visibility.print_with_space(cx.tcx(), myitem.def_id, cx.cache()),
                        anchor(myitem.def_id, &*myitem.name.as_ref().unwrap().as_str(), cx.cache()),
                    ),
                }
                w.write_str("</code></td></tr>");
            }

            clean::ImportItem(ref import) => {
                write!(
                    w,
                    "<tr><td><code>{}{}</code></td></tr>",
                    myitem.visibility.print_with_space(cx.tcx(), myitem.def_id, cx.cache()),
                    import.print(cx.cache(), cx.tcx()),
                );
            }

            _ => {
                if myitem.name.is_none() {
                    continue;
                }

                let unsafety_flag = match *myitem.kind {
                    clean::FunctionItem(ref func) | clean::ForeignFunctionItem(ref func)
                        if func.header.unsafety == hir::Unsafety::Unsafe =>
                    {
                        "<a title=\"unsafe function\" href=\"#\"><sup>⚠</sup></a>"
                    }
                    _ => "",
                };

                let stab = myitem.stability_class(cx.tcx());
                let add = if stab.is_some() { " " } else { "" };

                let doc_value = myitem.doc_value().unwrap_or_default();
                write!(
                    w,
                    "<tr class=\"{stab}{add}module-item\">\
                         <td><a class=\"{class}\" href=\"{href}\" \
                             title=\"{title}\">{name}</a>{unsafety_flag}</td>\
                         <td class=\"docblock-short\">{stab_tags}{docs}</td>\
                     </tr>",
                    name = *myitem.name.as_ref().unwrap(),
                    stab_tags = extra_info_tags(myitem, item, cx.tcx()),
                    docs = MarkdownSummaryLine(&doc_value, &myitem.links(&cx.cache)).into_string(),
                    class = myitem.type_(),
                    add = add,
                    stab = stab.unwrap_or_else(String::new),
                    unsafety_flag = unsafety_flag,
                    href = item_path(myitem.type_(), &myitem.name.unwrap().as_str()),
                    title = [full_path(cx, myitem), myitem.type_().to_string()]
                        .iter()
                        .filter_map(|s| if !s.is_empty() { Some(s.as_str()) } else { None })
                        .collect::<Vec<_>>()
                        .join(" "),
                );
            }
        }
    }

    if curty.is_some() {
        w.write_str("</table>");
    }
}

/// Render the stability, deprecation and portability tags that are displayed in the item's summary
/// at the module level.
fn extra_info_tags(item: &clean::Item, parent: &clean::Item, tcx: TyCtxt<'_>) -> String {
    let mut tags = String::new();

    fn tag_html(class: &str, title: &str, contents: &str) -> String {
        format!(r#"<span class="stab {}" title="{}">{}</span>"#, class, Escape(title), contents)
    }

    // The trailing space after each tag is to space it properly against the rest of the docs.
    if let Some(depr) = &item.deprecation(tcx) {
        let mut message = "Deprecated";
        if !stability::deprecation_in_effect(
            depr.is_since_rustc_version,
            depr.since.map(|s| s.as_str()).as_deref(),
        ) {
            message = "Deprecation planned";
        }
        tags += &tag_html("deprecated", "", message);
    }

    // The "rustc_private" crates are permanently unstable so it makes no sense
    // to render "unstable" everywhere.
    if item
        .stability(tcx)
        .as_ref()
        .map(|s| s.level.is_unstable() && s.feature != sym::rustc_private)
        == Some(true)
    {
        tags += &tag_html("unstable", "", "Experimental");
    }

    let cfg = match (&item.attrs.cfg, parent.attrs.cfg.as_ref()) {
        (Some(cfg), Some(parent_cfg)) => cfg.simplify_with(parent_cfg),
        (cfg, _) => cfg.as_deref().cloned(),
    };

    debug!("Portability {:?} - {:?} = {:?}", item.attrs.cfg, parent.attrs.cfg, cfg);
    if let Some(ref cfg) = cfg {
        tags += &tag_html("portability", &cfg.render_long_plain(), &cfg.render_short_html());
    }

    tags
}

fn item_function(w: &mut Buffer, cx: &Context<'_>, it: &clean::Item, f: &clean::Function) {
    let header_len = format!(
        "{}{}{}{}{:#}fn {}{:#}",
        it.visibility.print_with_space(cx.tcx(), it.def_id, cx.cache()),
        f.header.constness.print_with_space(),
        f.header.asyncness.print_with_space(),
        f.header.unsafety.print_with_space(),
        print_abi_with_space(f.header.abi),
        it.name.as_ref().unwrap(),
        f.generics.print(cx.cache(), cx.tcx())
    )
    .len();
    w.write_str("<pre class=\"rust fn\">");
    render_attributes(w, it, false);
    write!(
        w,
        "{vis}{constness}{asyncness}{unsafety}{abi}fn \
         {name}{generics}{decl}{notable_traits}{where_clause}</pre>",
        vis = it.visibility.print_with_space(cx.tcx(), it.def_id, cx.cache()),
        constness = f.header.constness.print_with_space(),
        asyncness = f.header.asyncness.print_with_space(),
        unsafety = f.header.unsafety.print_with_space(),
        abi = print_abi_with_space(f.header.abi),
        name = it.name.as_ref().unwrap(),
        generics = f.generics.print(cx.cache(), cx.tcx()),
        where_clause = print_where_clause(&f.generics, cx.cache(), cx.tcx(), 0, true),
        decl = f.decl.full_print(cx.cache(), cx.tcx(), header_len, 0, f.header.asyncness),
        notable_traits = notable_traits_decl(&f.decl, cx.cache(), cx.tcx()),
    );
    document(w, cx, it, None)
}

fn item_trait(w: &mut Buffer, cx: &Context<'_>, it: &clean::Item, t: &clean::Trait) {
    let bounds = bounds(&t.bounds, false, cx.cache(), cx.tcx());
    let types = t.items.iter().filter(|m| m.is_associated_type()).collect::<Vec<_>>();
    let consts = t.items.iter().filter(|m| m.is_associated_const()).collect::<Vec<_>>();
    let required = t.items.iter().filter(|m| m.is_ty_method()).collect::<Vec<_>>();
    let provided = t.items.iter().filter(|m| m.is_method()).collect::<Vec<_>>();

    // Output the trait definition
    wrap_into_docblock(w, |w| {
        w.write_str("<pre class=\"rust trait\">");
        render_attributes(w, it, true);
        write!(
            w,
            "{}{}{}trait {}{}{}",
            it.visibility.print_with_space(cx.tcx(), it.def_id, cx.cache()),
            t.unsafety.print_with_space(),
            if t.is_auto { "auto " } else { "" },
            it.name.as_ref().unwrap(),
            t.generics.print(cx.cache(), cx.tcx()),
            bounds
        );

        if !t.generics.where_predicates.is_empty() {
            write!(w, "{}", print_where_clause(&t.generics, cx.cache(), cx.tcx(), 0, true));
        } else {
            w.write_str(" ");
        }

        if t.items.is_empty() {
            w.write_str("{ }");
        } else {
            // FIXME: we should be using a derived_id for the Anchors here
            w.write_str("{\n");
            for t in &types {
                render_assoc_item(w, t, AssocItemLink::Anchor(None), ItemType::Trait, cx);
                w.write_str(";\n");
            }
            if !types.is_empty() && !consts.is_empty() {
                w.write_str("\n");
            }
            for t in &consts {
                render_assoc_item(w, t, AssocItemLink::Anchor(None), ItemType::Trait, cx);
                w.write_str(";\n");
            }
            if !consts.is_empty() && !required.is_empty() {
                w.write_str("\n");
            }
            for (pos, m) in required.iter().enumerate() {
                render_assoc_item(w, m, AssocItemLink::Anchor(None), ItemType::Trait, cx);
                w.write_str(";\n");

                if pos < required.len() - 1 {
                    w.write_str("<div class=\"item-spacer\"></div>");
                }
            }
            if !required.is_empty() && !provided.is_empty() {
                w.write_str("\n");
            }
            for (pos, m) in provided.iter().enumerate() {
                render_assoc_item(w, m, AssocItemLink::Anchor(None), ItemType::Trait, cx);
                match *m.kind {
                    clean::MethodItem(ref inner, _)
                        if !inner.generics.where_predicates.is_empty() =>
                    {
                        w.write_str(",\n    { ... }\n");
                    }
                    _ => {
                        w.write_str(" { ... }\n");
                    }
                }
                if pos < provided.len() - 1 {
                    w.write_str("<div class=\"item-spacer\"></div>");
                }
            }
            w.write_str("}");
        }
        w.write_str("</pre>")
    });

    // Trait documentation
    document(w, cx, it, None);

    fn write_small_section_header(w: &mut Buffer, id: &str, title: &str, extra_content: &str) {
        write!(
            w,
            "<h2 id=\"{0}\" class=\"small-section-header\">\
                {1}<a href=\"#{0}\" class=\"anchor\"></a>\
             </h2>{2}",
            id, title, extra_content
        )
    }

    fn write_loading_content(w: &mut Buffer, extra_content: &str) {
        write!(w, "{}<span class=\"loading-content\">Loading content...</span>", extra_content)
    }

    fn trait_item(w: &mut Buffer, cx: &Context<'_>, m: &clean::Item, t: &clean::Item) {
        let name = m.name.as_ref().unwrap();
        info!("Documenting {} on {:?}", name, t.name);
        let item_type = m.type_();
        let id = cx.derive_id(format!("{}.{}", item_type, name));
        write!(w, "<h3 id=\"{id}\" class=\"method\"><code>", id = id,);
        render_assoc_item(w, m, AssocItemLink::Anchor(Some(&id)), ItemType::Impl, cx);
        w.write_str("</code>");
        render_stability_since(w, m, t, cx.tcx());
        write_srclink(cx, m, w);
        w.write_str("</h3>");
        document(w, cx, m, Some(t));
    }

    if !types.is_empty() {
        write_small_section_header(
            w,
            "associated-types",
            "Associated Types",
            "<div class=\"methods\">",
        );
        for t in types {
            trait_item(w, cx, t, it);
        }
        write_loading_content(w, "</div>");
    }

    if !consts.is_empty() {
        write_small_section_header(
            w,
            "associated-const",
            "Associated Constants",
            "<div class=\"methods\">",
        );
        for t in consts {
            trait_item(w, cx, t, it);
        }
        write_loading_content(w, "</div>");
    }

    // Output the documentation for each function individually
    if !required.is_empty() {
        write_small_section_header(
            w,
            "required-methods",
            "Required methods",
            "<div class=\"methods\">",
        );
        for m in required {
            trait_item(w, cx, m, it);
        }
        write_loading_content(w, "</div>");
    }
    if !provided.is_empty() {
        write_small_section_header(
            w,
            "provided-methods",
            "Provided methods",
            "<div class=\"methods\">",
        );
        for m in provided {
            trait_item(w, cx, m, it);
        }
        write_loading_content(w, "</div>");
    }

    // If there are methods directly on this trait object, render them here.
    render_assoc_items(w, cx, it, it.def_id, AssocItemRender::All);

    if let Some(implementors) = cx.cache.implementors.get(&it.def_id) {
        // The DefId is for the first Type found with that name. The bool is
        // if any Types with the same name but different DefId have been found.
        let mut implementor_dups: FxHashMap<Symbol, (DefId, bool)> = FxHashMap::default();
        for implementor in implementors {
            match implementor.inner_impl().for_ {
                clean::ResolvedPath { ref path, did, is_generic: false, .. }
                | clean::BorrowedRef {
                    type_: box clean::ResolvedPath { ref path, did, is_generic: false, .. },
                    ..
                } => {
                    let &mut (prev_did, ref mut has_duplicates) =
                        implementor_dups.entry(path.last()).or_insert((did, false));
                    if prev_did != did {
                        *has_duplicates = true;
                    }
                }
                _ => {}
            }
        }

        let (local, foreign) = implementors.iter().partition::<Vec<_>, _>(|i| {
            i.inner_impl()
                .for_
                .def_id_full(cx.cache())
                .map_or(true, |d| cx.cache.paths.contains_key(&d))
        });

        let (mut synthetic, mut concrete): (Vec<&&Impl>, Vec<&&Impl>) =
            local.iter().partition(|i| i.inner_impl().synthetic);

        synthetic.sort_by(|a, b| compare_impl(a, b, cx.cache(), cx.tcx()));
        concrete.sort_by(|a, b| compare_impl(a, b, cx.cache(), cx.tcx()));

        if !foreign.is_empty() {
            write_small_section_header(w, "foreign-impls", "Implementations on Foreign Types", "");

            for implementor in foreign {
                let assoc_link = AssocItemLink::GotoSource(
                    implementor.impl_item.def_id,
                    &implementor.inner_impl().provided_trait_methods,
                );
                render_impl(
                    w,
                    cx,
                    &implementor,
                    it,
                    assoc_link,
                    RenderMode::Normal,
                    implementor.impl_item.stable_since(cx.tcx()).as_deref(),
                    implementor.impl_item.const_stable_since(cx.tcx()).as_deref(),
                    false,
                    None,
                    true,
                    false,
                    &[],
                );
            }
            write_loading_content(w, "");
        }

        write_small_section_header(
            w,
            "implementors",
            "Implementors",
            "<div class=\"item-list\" id=\"implementors-list\">",
        );
        for implementor in concrete {
            render_implementor(cx, implementor, it, w, &implementor_dups, &[]);
        }
        write_loading_content(w, "</div>");

        if t.is_auto {
            write_small_section_header(
                w,
                "synthetic-implementors",
                "Auto implementors",
                "<div class=\"item-list\" id=\"synthetic-implementors-list\">",
            );
            for implementor in synthetic {
                render_implementor(
                    cx,
                    implementor,
                    it,
                    w,
                    &implementor_dups,
                    &collect_paths_for_type(implementor.inner_impl().for_.clone(), &cx.cache),
                );
            }
            write_loading_content(w, "</div>");
        }
    } else {
        // even without any implementations to write in, we still want the heading and list, so the
        // implementors javascript file pulled in below has somewhere to write the impls into
        write_small_section_header(
            w,
            "implementors",
            "Implementors",
            "<div class=\"item-list\" id=\"implementors-list\">",
        );
        write_loading_content(w, "</div>");

        if t.is_auto {
            write_small_section_header(
                w,
                "synthetic-implementors",
                "Auto implementors",
                "<div class=\"item-list\" id=\"synthetic-implementors-list\">",
            );
            write_loading_content(w, "</div>");
        }
    }

    write!(
        w,
        "<script type=\"text/javascript\" \
                 src=\"{root_path}/implementors/{path}/{ty}.{name}.js\" async>\
         </script>",
        root_path = vec![".."; cx.current.len()].join("/"),
        path = if it.def_id.is_local() {
            cx.current.join("/")
        } else {
            let (ref path, _) = cx.cache.external_paths[&it.def_id];
            path[..path.len() - 1].join("/")
        },
        ty = it.type_(),
        name = *it.name.as_ref().unwrap()
    );
}

fn item_trait_alias(w: &mut Buffer, cx: &Context<'_>, it: &clean::Item, t: &clean::TraitAlias) {
    w.write_str("<pre class=\"rust trait-alias\">");
    render_attributes(w, it, false);
    write!(
        w,
        "trait {}{}{} = {};</pre>",
        it.name.as_ref().unwrap(),
        t.generics.print(cx.cache(), cx.tcx()),
        print_where_clause(&t.generics, cx.cache(), cx.tcx(), 0, true),
        bounds(&t.bounds, true, cx.cache(), cx.tcx())
    );

    document(w, cx, it, None);

    // Render any items associated directly to this alias, as otherwise they
    // won't be visible anywhere in the docs. It would be nice to also show
    // associated items from the aliased type (see discussion in #32077), but
    // we need #14072 to make sense of the generics.
    render_assoc_items(w, cx, it, it.def_id, AssocItemRender::All)
}

fn item_opaque_ty(w: &mut Buffer, cx: &Context<'_>, it: &clean::Item, t: &clean::OpaqueTy) {
    w.write_str("<pre class=\"rust opaque\">");
    render_attributes(w, it, false);
    write!(
        w,
        "type {}{}{where_clause} = impl {bounds};</pre>",
        it.name.as_ref().unwrap(),
        t.generics.print(cx.cache(), cx.tcx()),
        where_clause = print_where_clause(&t.generics, cx.cache(), cx.tcx(), 0, true),
        bounds = bounds(&t.bounds, false, cx.cache(), cx.tcx()),
    );

    document(w, cx, it, None);

    // Render any items associated directly to this alias, as otherwise they
    // won't be visible anywhere in the docs. It would be nice to also show
    // associated items from the aliased type (see discussion in #32077), but
    // we need #14072 to make sense of the generics.
    render_assoc_items(w, cx, it, it.def_id, AssocItemRender::All)
}

fn item_typedef(w: &mut Buffer, cx: &Context<'_>, it: &clean::Item, t: &clean::Typedef) {
    w.write_str("<pre class=\"rust typedef\">");
    render_attributes(w, it, false);
    write!(
        w,
        "type {}{}{where_clause} = {type_};</pre>",
        it.name.as_ref().unwrap(),
        t.generics.print(cx.cache(), cx.tcx()),
        where_clause = print_where_clause(&t.generics, cx.cache(), cx.tcx(), 0, true),
        type_ = t.type_.print(cx.cache(), cx.tcx()),
    );

    document(w, cx, it, None);

    // Render any items associated directly to this alias, as otherwise they
    // won't be visible anywhere in the docs. It would be nice to also show
    // associated items from the aliased type (see discussion in #32077), but
    // we need #14072 to make sense of the generics.
    render_assoc_items(w, cx, it, it.def_id, AssocItemRender::All)
}

fn item_union(w: &mut Buffer, cx: &Context<'_>, it: &clean::Item, s: &clean::Union) {
    wrap_into_docblock(w, |w| {
        w.write_str("<pre class=\"rust union\">");
        render_attributes(w, it, true);
        render_union(w, it, Some(&s.generics), &s.fields, "", true, cx);
        w.write_str("</pre>")
    });

    document(w, cx, it, None);
    let mut fields = s
        .fields
        .iter()
        .filter_map(|f| match *f.kind {
            clean::StructFieldItem(ref ty) => Some((f, ty)),
            _ => None,
        })
        .peekable();
    if fields.peek().is_some() {
        write!(
            w,
            "<h2 id=\"fields\" class=\"fields small-section-header\">
                   Fields<a href=\"#fields\" class=\"anchor\"></a></h2>"
        );
        for (field, ty) in fields {
            let name = field.name.as_ref().expect("union field name");
            let id = format!("{}.{}", ItemType::StructField, name);
            write!(
                w,
                "<span id=\"{id}\" class=\"{shortty} small-section-header\">\
                     <a href=\"#{id}\" class=\"anchor field\"></a>\
                     <code>{name}: {ty}</code>\
                 </span>",
                id = id,
                name = name,
                shortty = ItemType::StructField,
                ty = ty.print(cx.cache(), cx.tcx()),
            );
            if let Some(stability_class) = field.stability_class(cx.tcx()) {
                write!(w, "<span class=\"stab {stab}\"></span>", stab = stability_class);
            }
            document(w, cx, field, Some(it));
        }
    }
    render_assoc_items(w, cx, it, it.def_id, AssocItemRender::All)
}

fn item_enum(w: &mut Buffer, cx: &Context<'_>, it: &clean::Item, e: &clean::Enum) {
    wrap_into_docblock(w, |w| {
        w.write_str("<pre class=\"rust enum\">");
        render_attributes(w, it, true);
        write!(
            w,
            "{}enum {}{}{}",
            it.visibility.print_with_space(cx.tcx(), it.def_id, cx.cache()),
            it.name.as_ref().unwrap(),
            e.generics.print(cx.cache(), cx.tcx()),
            print_where_clause(&e.generics, cx.cache(), cx.tcx(), 0, true),
        );
        if e.variants.is_empty() && !e.variants_stripped {
            w.write_str(" {}");
        } else {
            w.write_str(" {\n");
            for v in &e.variants {
                w.write_str("    ");
                let name = v.name.as_ref().unwrap();
                match *v.kind {
                    clean::VariantItem(ref var) => match var {
                        clean::Variant::CLike => write!(w, "{}", name),
                        clean::Variant::Tuple(ref tys) => {
                            write!(w, "{}(", name);
                            for (i, ty) in tys.iter().enumerate() {
                                if i > 0 {
                                    w.write_str(",&nbsp;")
                                }
                                write!(w, "{}", ty.print(cx.cache(), cx.tcx()));
                            }
                            w.write_str(")");
                        }
                        clean::Variant::Struct(ref s) => {
                            render_struct(w, v, None, s.struct_type, &s.fields, "    ", false, cx);
                        }
                    },
                    _ => unreachable!(),
                }
                w.write_str(",\n");
            }

            if e.variants_stripped {
                w.write_str("    // some variants omitted\n");
            }
            w.write_str("}");
        }
        w.write_str("</pre>")
    });

    document(w, cx, it, None);
    if !e.variants.is_empty() {
        write!(
            w,
            "<h2 id=\"variants\" class=\"variants small-section-header\">
                   Variants{}<a href=\"#variants\" class=\"anchor\"></a></h2>\n",
            document_non_exhaustive_header(it)
        );
        document_non_exhaustive(w, it);
        for variant in &e.variants {
            let id =
                cx.derive_id(format!("{}.{}", ItemType::Variant, variant.name.as_ref().unwrap()));
            write!(
                w,
                "<div id=\"{id}\" class=\"variant small-section-header\">\
                    <a href=\"#{id}\" class=\"anchor field\"></a>\
                    <code>{name}",
                id = id,
                name = variant.name.as_ref().unwrap()
            );
            if let clean::VariantItem(clean::Variant::Tuple(ref tys)) = *variant.kind {
                w.write_str("(");
                for (i, ty) in tys.iter().enumerate() {
                    if i > 0 {
                        w.write_str(",&nbsp;");
                    }
                    write!(w, "{}", ty.print(cx.cache(), cx.tcx()));
                }
                w.write_str(")");
            }
            w.write_str("</code></div>");
            document(w, cx, variant, Some(it));
            document_non_exhaustive(w, variant);

            use crate::clean::Variant;
            if let clean::VariantItem(Variant::Struct(ref s)) = *variant.kind {
                let variant_id = cx.derive_id(format!(
                    "{}.{}.fields",
                    ItemType::Variant,
                    variant.name.as_ref().unwrap()
                ));
                write!(w, "<div class=\"autohide sub-variant\" id=\"{id}\">", id = variant_id);
                write!(
                    w,
                    "<h3>Fields of <b>{name}</b></h3><div>",
                    name = variant.name.as_ref().unwrap()
                );
                for field in &s.fields {
                    use crate::clean::StructFieldItem;
                    if let StructFieldItem(ref ty) = *field.kind {
                        let id = cx.derive_id(format!(
                            "variant.{}.field.{}",
                            variant.name.as_ref().unwrap(),
                            field.name.as_ref().unwrap()
                        ));
                        write!(
                            w,
                            "<span id=\"{id}\" class=\"variant small-section-header\">\
                                 <a href=\"#{id}\" class=\"anchor field\"></a>\
                                 <code>{f}:&nbsp;{t}</code>\
                             </span>",
                            id = id,
                            f = field.name.as_ref().unwrap(),
                            t = ty.print(cx.cache(), cx.tcx())
                        );
                        document(w, cx, field, Some(variant));
                    }
                }
                w.write_str("</div></div>");
            }
            render_stability_since(w, variant, it, cx.tcx());
        }
    }
    render_assoc_items(w, cx, it, it.def_id, AssocItemRender::All)
}

fn item_macro(w: &mut Buffer, cx: &Context<'_>, it: &clean::Item, t: &clean::Macro) {
    wrap_into_docblock(w, |w| {
        highlight::render_with_highlighting(
            &t.source,
            w,
            Some("macro"),
            None,
            None,
            it.span.inner().edition(),
        );
    });
    document(w, cx, it, None)
}

fn item_proc_macro(w: &mut Buffer, cx: &Context<'_>, it: &clean::Item, m: &clean::ProcMacro) {
    let name = it.name.as_ref().expect("proc-macros always have names");
    match m.kind {
        MacroKind::Bang => {
            w.push_str("<pre class=\"rust macro\">");
            write!(w, "{}!() {{ /* proc-macro */ }}", name);
            w.push_str("</pre>");
        }
        MacroKind::Attr => {
            w.push_str("<pre class=\"rust attr\">");
            write!(w, "#[{}]", name);
            w.push_str("</pre>");
        }
        MacroKind::Derive => {
            w.push_str("<pre class=\"rust derive\">");
            write!(w, "#[derive({})]", name);
            if !m.helpers.is_empty() {
                w.push_str("\n{\n");
                w.push_str("    // Attributes available to this derive:\n");
                for attr in &m.helpers {
                    writeln!(w, "    #[{}]", attr);
                }
                w.push_str("}\n");
            }
            w.push_str("</pre>");
        }
    }
    document(w, cx, it, None)
}

fn item_primitive(w: &mut Buffer, cx: &Context<'_>, it: &clean::Item) {
    document(w, cx, it, None);
    render_assoc_items(w, cx, it, it.def_id, AssocItemRender::All)
}

fn item_constant(w: &mut Buffer, cx: &Context<'_>, it: &clean::Item, c: &clean::Constant) {
    w.write_str("<pre class=\"rust const\">");
    render_attributes(w, it, false);

    write!(
        w,
        "{vis}const {name}: {typ}",
        vis = it.visibility.print_with_space(cx.tcx(), it.def_id, cx.cache()),
        name = it.name.as_ref().unwrap(),
        typ = c.type_.print(cx.cache(), cx.tcx()),
    );

    let value = c.value(cx.tcx());
    let is_literal = c.is_literal(cx.tcx());
    let expr = c.expr(cx.tcx());
    if value.is_some() || is_literal {
        write!(w, " = {expr};", expr = Escape(&expr));
    } else {
        w.write_str(";");
    }

    if !is_literal {
        if let Some(value) = &value {
            let value_lowercase = value.to_lowercase();
            let expr_lowercase = expr.to_lowercase();

            if value_lowercase != expr_lowercase
                && value_lowercase.trim_end_matches("i32") != expr_lowercase
            {
                write!(w, " // {value}", value = Escape(value));
            }
        }
    }

    w.write_str("</pre>");
    document(w, cx, it, None)
}

fn item_struct(w: &mut Buffer, cx: &Context<'_>, it: &clean::Item, s: &clean::Struct) {
    wrap_into_docblock(w, |w| {
        w.write_str("<pre class=\"rust struct\">");
        render_attributes(w, it, true);
        render_struct(w, it, Some(&s.generics), s.struct_type, &s.fields, "", true, cx);
        w.write_str("</pre>")
    });

    document(w, cx, it, None);
    let mut fields = s
        .fields
        .iter()
        .filter_map(|f| match *f.kind {
            clean::StructFieldItem(ref ty) => Some((f, ty)),
            _ => None,
        })
        .peekable();
    if let CtorKind::Fictive = s.struct_type {
        if fields.peek().is_some() {
            write!(
                w,
                "<h2 id=\"fields\" class=\"fields small-section-header\">
                       Fields{}<a href=\"#fields\" class=\"anchor\"></a></h2>",
                document_non_exhaustive_header(it)
            );
            document_non_exhaustive(w, it);
            for (field, ty) in fields {
                let id = cx.derive_id(format!(
                    "{}.{}",
                    ItemType::StructField,
                    field.name.as_ref().unwrap()
                ));
                write!(
                    w,
                    "<span id=\"{id}\" class=\"{item_type} small-section-header\">\
                         <a href=\"#{id}\" class=\"anchor field\"></a>\
                         <code>{name}: {ty}</code>\
                     </span>",
                    item_type = ItemType::StructField,
                    id = id,
                    name = field.name.as_ref().unwrap(),
                    ty = ty.print(cx.cache(), cx.tcx())
                );
                document(w, cx, field, Some(it));
            }
        }
    }
    render_assoc_items(w, cx, it, it.def_id, AssocItemRender::All)
}

fn item_static(w: &mut Buffer, cx: &Context<'_>, it: &clean::Item, s: &clean::Static) {
    w.write_str("<pre class=\"rust static\">");
    render_attributes(w, it, false);
    write!(
        w,
        "{vis}static {mutability}{name}: {typ}</pre>",
        vis = it.visibility.print_with_space(cx.tcx(), it.def_id, cx.cache()),
        mutability = s.mutability.print_with_space(),
        name = it.name.as_ref().unwrap(),
        typ = s.type_.print(cx.cache(), cx.tcx())
    );
    document(w, cx, it, None)
}

fn item_foreign_type(w: &mut Buffer, cx: &Context<'_>, it: &clean::Item) {
    w.write_str("<pre class=\"rust foreigntype\">extern {\n");
    render_attributes(w, it, false);
    write!(
        w,
        "    {}type {};\n}}</pre>",
        it.visibility.print_with_space(cx.tcx(), it.def_id, cx.cache()),
        it.name.as_ref().unwrap(),
    );

    document(w, cx, it, None);

    render_assoc_items(w, cx, it, it.def_id, AssocItemRender::All)
}

fn item_keyword(w: &mut Buffer, cx: &Context<'_>, it: &clean::Item) {
    document(w, cx, it, None)
}

/// Compare two strings treating multi-digit numbers as single units (i.e. natural sort order).
crate fn compare_names(mut lhs: &str, mut rhs: &str) -> Ordering {
    /// Takes a non-numeric and a numeric part from the given &str.
    fn take_parts<'a>(s: &mut &'a str) -> (&'a str, &'a str) {
        let i = s.find(|c: char| c.is_ascii_digit());
        let (a, b) = s.split_at(i.unwrap_or(s.len()));
        let i = b.find(|c: char| !c.is_ascii_digit());
        let (b, c) = b.split_at(i.unwrap_or(b.len()));
        *s = c;
        (a, b)
    }

    while !lhs.is_empty() || !rhs.is_empty() {
        let (la, lb) = take_parts(&mut lhs);
        let (ra, rb) = take_parts(&mut rhs);
        // First process the non-numeric part.
        match la.cmp(ra) {
            Ordering::Equal => (),
            x => return x,
        }
        // Then process the numeric part, if both sides have one (and they fit in a u64).
        if let (Ok(ln), Ok(rn)) = (lb.parse::<u64>(), rb.parse::<u64>()) {
            match ln.cmp(&rn) {
                Ordering::Equal => (),
                x => return x,
            }
        }
        // Then process the numeric part again, but this time as strings.
        match lb.cmp(rb) {
            Ordering::Equal => (),
            x => return x,
        }
    }

    Ordering::Equal
}

pub(super) fn full_path(cx: &Context<'_>, item: &clean::Item) -> String {
    let mut s = cx.current.join("::");
    s.push_str("::");
    s.push_str(&item.name.unwrap().as_str());
    s
}

pub(super) fn item_path(ty: ItemType, name: &str) -> String {
    match ty {
        ItemType::Module => format!("{}index.html", ensure_trailing_slash(name)),
        _ => format!("{}.{}.html", ty, name),
    }
}

fn bounds(
    t_bounds: &[clean::GenericBound],
    trait_alias: bool,
    cache: &Cache,
    tcx: TyCtxt<'_>,
) -> String {
    let mut bounds = String::new();
    if !t_bounds.is_empty() {
        if !trait_alias {
            bounds.push_str(": ");
        }
        for (i, p) in t_bounds.iter().enumerate() {
            if i > 0 {
                bounds.push_str(" + ");
            }
            bounds.push_str(&p.print(cache, tcx).to_string());
        }
    }
    bounds
}

fn wrap_into_docblock<F>(w: &mut Buffer, f: F)
where
    F: FnOnce(&mut Buffer),
{
    w.write_str("<div class=\"docblock type-decl hidden-by-usual-hider\">");
    f(w);
    w.write_str("</div>")
}

fn render_stability_since(
    w: &mut Buffer,
    item: &clean::Item,
    containing_item: &clean::Item,
    tcx: TyCtxt<'_>,
) {
    render_stability_since_raw(
        w,
        item.stable_since(tcx).as_deref(),
        item.const_stable_since(tcx).as_deref(),
        containing_item.stable_since(tcx).as_deref(),
        containing_item.const_stable_since(tcx).as_deref(),
    )
}

fn compare_impl<'a, 'b>(
    lhs: &'a &&Impl,
    rhs: &'b &&Impl,
    cache: &Cache,
    tcx: TyCtxt<'_>,
) -> Ordering {
    let lhs = format!("{}", lhs.inner_impl().print(cache, false, tcx));
    let rhs = format!("{}", rhs.inner_impl().print(cache, false, tcx));

    // lhs and rhs are formatted as HTML, which may be unnecessary
    compare_names(&lhs, &rhs)
}

fn render_implementor(
    cx: &Context<'_>,
    implementor: &Impl,
    trait_: &clean::Item,
    w: &mut Buffer,
    implementor_dups: &FxHashMap<Symbol, (DefId, bool)>,
    aliases: &[String],
) {
    // If there's already another implementor that has the same abbridged name, use the
    // full path, for example in `std::iter::ExactSizeIterator`
    let use_absolute = match implementor.inner_impl().for_ {
        clean::ResolvedPath { ref path, is_generic: false, .. }
        | clean::BorrowedRef {
            type_: box clean::ResolvedPath { ref path, is_generic: false, .. },
            ..
        } => implementor_dups[&path.last()].1,
        _ => false,
    };
    render_impl(
        w,
        cx,
        implementor,
        trait_,
        AssocItemLink::Anchor(None),
        RenderMode::Normal,
        trait_.stable_since(cx.tcx()).as_deref(),
        trait_.const_stable_since(cx.tcx()).as_deref(),
        false,
        Some(use_absolute),
        false,
        false,
        aliases,
    );
}

fn render_union(
    w: &mut Buffer,
    it: &clean::Item,
    g: Option<&clean::Generics>,
    fields: &[clean::Item],
    tab: &str,
    structhead: bool,
    cx: &Context<'_>,
) {
    write!(
        w,
        "{}{}{}",
        it.visibility.print_with_space(cx.tcx(), it.def_id, cx.cache()),
        if structhead { "union " } else { "" },
        it.name.as_ref().unwrap()
    );
    if let Some(g) = g {
        write!(w, "{}", g.print(cx.cache(), cx.tcx()));
        write!(w, "{}", print_where_clause(&g, cx.cache(), cx.tcx(), 0, true));
    }

    write!(w, " {{\n{}", tab);
    for field in fields {
        if let clean::StructFieldItem(ref ty) = *field.kind {
            write!(
                w,
                "    {}{}: {},\n{}",
                field.visibility.print_with_space(cx.tcx(), field.def_id, cx.cache()),
                field.name.as_ref().unwrap(),
                ty.print(cx.cache(), cx.tcx()),
                tab
            );
        }
    }

    if it.has_stripped_fields().unwrap() {
        write!(w, "    // some fields omitted\n{}", tab);
    }
    w.write_str("}");
}

fn render_struct(
    w: &mut Buffer,
    it: &clean::Item,
    g: Option<&clean::Generics>,
    ty: CtorKind,
    fields: &[clean::Item],
    tab: &str,
    structhead: bool,
    cx: &Context<'_>,
) {
    write!(
        w,
        "{}{}{}",
        it.visibility.print_with_space(cx.tcx(), it.def_id, cx.cache()),
        if structhead { "struct " } else { "" },
        it.name.as_ref().unwrap()
    );
    if let Some(g) = g {
        write!(w, "{}", g.print(cx.cache(), cx.tcx()))
    }
    match ty {
        CtorKind::Fictive => {
            if let Some(g) = g {
                write!(w, "{}", print_where_clause(g, cx.cache(), cx.tcx(), 0, true),)
            }
            let mut has_visible_fields = false;
            w.write_str(" {");
            for field in fields {
                if let clean::StructFieldItem(ref ty) = *field.kind {
                    write!(
                        w,
                        "\n{}    {}{}: {},",
                        tab,
                        field.visibility.print_with_space(cx.tcx(), field.def_id, cx.cache()),
                        field.name.as_ref().unwrap(),
                        ty.print(cx.cache(), cx.tcx()),
                    );
                    has_visible_fields = true;
                }
            }

            if has_visible_fields {
                if it.has_stripped_fields().unwrap() {
                    write!(w, "\n{}    // some fields omitted", tab);
                }
                write!(w, "\n{}", tab);
            } else if it.has_stripped_fields().unwrap() {
                // If there are no visible fields we can just display
                // `{ /* fields omitted */ }` to save space.
                write!(w, " /* fields omitted */ ");
            }
            w.write_str("}");
        }
        CtorKind::Fn => {
            w.write_str("(");
            for (i, field) in fields.iter().enumerate() {
                if i > 0 {
                    w.write_str(", ");
                }
                match *field.kind {
                    clean::StrippedItem(box clean::StructFieldItem(..)) => write!(w, "_"),
                    clean::StructFieldItem(ref ty) => {
                        write!(
                            w,
                            "{}{}",
                            field.visibility.print_with_space(cx.tcx(), field.def_id, cx.cache()),
                            ty.print(cx.cache(), cx.tcx()),
                        )
                    }
                    _ => unreachable!(),
                }
            }
            w.write_str(")");
            if let Some(g) = g {
                write!(w, "{}", print_where_clause(g, cx.cache(), cx.tcx(), 0, false),)
            }
            w.write_str(";");
        }
        CtorKind::Const => {
            // Needed for PhantomData.
            if let Some(g) = g {
                write!(w, "{}", print_where_clause(g, cx.cache(), cx.tcx(), 0, false),)
            }
            w.write_str(";");
        }
    }
}

fn document_non_exhaustive_header(item: &clean::Item) -> &str {
    if item.is_non_exhaustive() { " (Non-exhaustive)" } else { "" }
}

fn document_non_exhaustive(w: &mut Buffer, item: &clean::Item) {
    if item.is_non_exhaustive() {
        write!(w, "<div class=\"docblock non-exhaustive non-exhaustive-{}\">", {
            if item.is_struct() {
                "struct"
            } else if item.is_enum() {
                "enum"
            } else if item.is_variant() {
                "variant"
            } else {
                "type"
            }
        });

        if item.is_struct() {
            w.write_str(
                "Non-exhaustive structs could have additional fields added in future. \
                 Therefore, non-exhaustive structs cannot be constructed in external crates \
                 using the traditional <code>Struct {{ .. }}</code> syntax; cannot be \
                 matched against without a wildcard <code>..</code>; and \
                 struct update syntax will not work.",
            );
        } else if item.is_enum() {
            w.write_str(
                "Non-exhaustive enums could have additional variants added in future. \
                 Therefore, when matching against variants of non-exhaustive enums, an \
                 extra wildcard arm must be added to account for any future variants.",
            );
        } else if item.is_variant() {
            w.write_str(
                "Non-exhaustive enum variants could have additional fields added in future. \
                 Therefore, non-exhaustive enum variants cannot be constructed in external \
                 crates and cannot be matched against.",
            );
        } else {
            w.write_str(
                "This type will require a wildcard arm in any match statements or constructors.",
            );
        }

        w.write_str("</div>");
    }
}
