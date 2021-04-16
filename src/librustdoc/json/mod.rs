//! Rustdoc's JSON backend
//!
//! This module contains the logic for rendering a crate as JSON rather than the normal static HTML
//! output. See [the RFC](https://github.com/rust-lang/rfcs/pull/2963) and the [`types`] module
//! docs for usage and details.

mod conversions;

use std::cell::RefCell;
use std::fs::File;
use std::path::PathBuf;
use std::rc::Rc;

use rustc_data_structures::fx::FxHashMap;
use rustc_middle::ty::TyCtxt;
use rustc_session::Session;
use rustc_span::{edition::Edition, Symbol};

use rustdoc_json_types as types;

use crate::clean;
use crate::config::RenderOptions;
use crate::error::Error;
use crate::formats::cache::Cache;
use crate::formats::FormatRenderer;
use crate::html::render::cache::ExternalLocation;
use crate::json::conversions::{from_def_id, IntoWithTcx};

#[derive(Clone)]
crate struct JsonRenderer<'tcx> {
    tcx: TyCtxt<'tcx>,
    /// A mapping of IDs that contains all local items for this crate which gets output as a top
    /// level field of the JSON blob.
    index: Rc<RefCell<FxHashMap<types::Id, types::Item>>>,
    /// The directory where the blob will be written to.
    out_path: PathBuf,
    cache: Rc<Cache>,
}

impl JsonRenderer<'tcx> {
    fn sess(&self) -> &'tcx Session {
        self.tcx.sess
    }

    fn get_trait_implementors(&mut self, id: rustc_span::def_id::DefId) -> Vec<types::Id> {
        Rc::clone(&self.cache)
            .implementors
            .get(&id)
            .map(|implementors| {
                implementors
                    .iter()
                    .map(|i| {
                        let item = &i.impl_item;
                        self.item(item.clone()).unwrap();
                        from_def_id(item.def_id)
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    fn get_impls(&mut self, id: rustc_span::def_id::DefId) -> Vec<types::Id> {
        Rc::clone(&self.cache)
            .impls
            .get(&id)
            .map(|impls| {
                impls
                    .iter()
                    .filter_map(|i| {
                        let item = &i.impl_item;
                        if item.def_id.is_local() {
                            self.item(item.clone()).unwrap();
                            Some(from_def_id(item.def_id))
                        } else {
                            None
                        }
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    fn get_trait_items(&mut self) -> Vec<(types::Id, types::Item)> {
        Rc::clone(&self.cache)
            .traits
            .iter()
            .filter_map(|(&id, trait_item)| {
                // only need to synthesize items for external traits
                if !id.is_local() {
                    let trait_item = &trait_item.trait_;
                    trait_item.items.clone().into_iter().for_each(|i| self.item(i).unwrap());
                    Some((
                        from_def_id(id),
                        types::Item {
                            id: from_def_id(id),
                            crate_id: id.krate.as_u32(),
                            name: self
                                .cache
                                .paths
                                .get(&id)
                                .unwrap_or_else(|| {
                                    self.cache
                                        .external_paths
                                        .get(&id)
                                        .expect("Trait should either be in local or external paths")
                                })
                                .0
                                .last()
                                .map(Clone::clone),
                            visibility: types::Visibility::Public,
                            inner: types::ItemEnum::Trait(trait_item.clone().into_tcx(self.tcx)),
                            span: None,
                            docs: Default::default(),
                            links: Default::default(),
                            attrs: Default::default(),
                            deprecation: Default::default(),
                        },
                    ))
                } else {
                    None
                }
            })
            .collect()
    }
}

impl<'tcx> FormatRenderer<'tcx> for JsonRenderer<'tcx> {
    fn descr() -> &'static str {
        "json"
    }

    const RUN_ON_MODULE: bool = false;

    fn init(
        krate: clean::Crate,
        options: RenderOptions,
        _edition: Edition,
        cache: Cache,
        tcx: TyCtxt<'tcx>,
    ) -> Result<(Self, clean::Crate), Error> {
        debug!("Initializing json renderer");
        Ok((
            JsonRenderer {
                tcx,
                index: Rc::new(RefCell::new(FxHashMap::default())),
                out_path: options.output,
                cache: Rc::new(cache),
            },
            krate,
        ))
    }

    fn make_child_renderer(&self) -> Self {
        self.clone()
    }

    /// Inserts an item into the index. This should be used rather than directly calling insert on
    /// the hashmap because certain items (traits and types) need to have their mappings for trait
    /// implementations filled out before they're inserted.
    fn item(&mut self, item: clean::Item) -> Result<(), Error> {
        // Flatten items that recursively store other items
        item.kind.inner_items().for_each(|i| self.item(i.clone()).unwrap());

        let id = item.def_id;
        if let Some(mut new_item) = self.convert_item(item) {
            if let types::ItemEnum::Trait(ref mut t) = new_item.inner {
                t.implementors = self.get_trait_implementors(id)
            } else if let types::ItemEnum::Struct(ref mut s) = new_item.inner {
                s.impls = self.get_impls(id)
            } else if let types::ItemEnum::Enum(ref mut e) = new_item.inner {
                e.impls = self.get_impls(id)
            }
            let removed = self.index.borrow_mut().insert(from_def_id(id), new_item.clone());

            // FIXME(adotinthevoid): Currently, the index is duplicated. This is a sanity check
            // to make sure the items are unique. The main place this happens is when an item, is
            // reexported in more than one place. See `rustdoc-json/reexport/in_root_and_mod`
            if let Some(old_item) = removed {
                assert_eq!(old_item, new_item);
            }
        }

        Ok(())
    }

    fn mod_item_in(&mut self, item: &clean::Item, _module_name: &str) -> Result<(), Error> {
        use clean::types::ItemKind::*;
        if let ModuleItem(m) = &*item.kind {
            for item in &m.items {
                match &*item.kind {
                    // These don't have names so they don't get added to the output by default
                    ImportItem(_) => self.item(item.clone()).unwrap(),
                    ExternCrateItem { .. } => self.item(item.clone()).unwrap(),
                    ImplItem(i) => i.items.iter().for_each(|i| self.item(i.clone()).unwrap()),
                    _ => {}
                }
            }
        }
        self.item(item.clone()).unwrap();
        Ok(())
    }

    fn mod_item_out(&mut self, _item_name: &str) -> Result<(), Error> {
        Ok(())
    }

    fn after_krate(
        &mut self,
        _crate_name: Symbol,
        _diag: &rustc_errors::Handler,
    ) -> Result<(), Error> {
        debug!("Done with crate");
        let mut index = (*self.index).clone().into_inner();
        index.extend(self.get_trait_items());
        // This needs to be the default HashMap for compatibility with the public interface for
        // rustdoc-json
        #[allow(rustc::default_hash_types)]
        let output = types::Crate {
            root: types::Id(String::from("0:0")),
            crate_version: self.cache.crate_version.clone(),
            includes_private: self.cache.document_private,
            index: index.into_iter().collect(),
            paths: self
                .cache
                .paths
                .clone()
                .into_iter()
                .chain(self.cache.external_paths.clone().into_iter())
                .map(|(k, (path, kind))| {
                    (
                        from_def_id(k),
                        types::ItemSummary {
                            crate_id: k.krate.as_u32(),
                            path,
                            kind: kind.into_tcx(self.tcx),
                        },
                    )
                })
                .collect(),
            external_crates: self
                .cache
                .extern_locations
                .iter()
                .map(|(k, v)| {
                    (
                        k.as_u32(),
                        types::ExternalCrate {
                            name: v.0.to_string(),
                            html_root_url: match &v.2 {
                                ExternalLocation::Remote(s) => Some(s.clone()),
                                _ => None,
                            },
                        },
                    )
                })
                .collect(),
            format_version: 5,
        };
        let mut p = self.out_path.clone();
        p.push(output.index.get(&output.root).unwrap().name.clone().unwrap());
        p.set_extension("json");
        let file = File::create(&p).map_err(|error| Error { error: error.to_string(), file: p })?;
        serde_json::ser::to_writer(&file, &output).unwrap();
        Ok(())
    }

    fn cache(&self) -> &Cache {
        &self.cache
    }
}
