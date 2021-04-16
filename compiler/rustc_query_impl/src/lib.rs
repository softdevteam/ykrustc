//! Support for serializing the dep-graph and reloading it.

#![doc(html_root_url = "https://doc.rust-lang.org/nightly/nightly-rustc/")]
#![feature(in_band_lifetimes)]
#![feature(exhaustive_patterns)]
#![feature(nll)]
#![feature(min_specialization)]
#![feature(crate_visibility_modifier)]
#![feature(once_cell)]
#![feature(rustc_attrs)]
#![feature(never_type)]
#![recursion_limit = "256"]

#[macro_use]
extern crate rustc_middle;
#[macro_use]
extern crate tracing;

use rustc_data_structures::fingerprint::Fingerprint;
use rustc_data_structures::stable_hasher::{HashStable, StableHasher};
use rustc_errors::{DiagnosticBuilder, Handler};
use rustc_hir::def_id::LOCAL_CRATE;
use rustc_middle::dep_graph;
use rustc_middle::ich::StableHashingContext;
use rustc_middle::ty::query::{query_keys, query_storage, query_stored, query_values};
use rustc_middle::ty::query::{Providers, QueryEngine};
use rustc_middle::ty::{self, TyCtxt};
use rustc_serialize::opaque;
use rustc_span::{Span, DUMMY_SP};

#[macro_use]
mod plumbing;
pub use plumbing::QueryCtxt;
use plumbing::QueryStruct;
use rustc_query_system::query::*;

mod stats;
pub use self::stats::print_stats;

mod keys;
use keys::Key;

mod values;
use self::values::Value;

use rustc_query_system::query::QueryAccessors;
pub use rustc_query_system::query::QueryConfig;
pub(crate) use rustc_query_system::query::QueryDescription;

use rustc_middle::ty::query::on_disk_cache;

mod profiling_support;
pub use self::profiling_support::alloc_self_profile_query_strings;

rustc_query_append! { [define_queries!][<'tcx>] }

impl<'tcx> Queries<'tcx> {
    // Force codegen in the dyn-trait transformation in this crate.
    pub fn as_dyn(&'tcx self) -> &'tcx dyn QueryEngine<'tcx> {
        self
    }
}
