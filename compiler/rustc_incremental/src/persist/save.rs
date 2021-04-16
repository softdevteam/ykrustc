use rustc_data_structures::fx::FxHashMap;
use rustc_data_structures::sync::join;
use rustc_middle::dep_graph::{DepGraph, PreviousDepGraph, WorkProduct, WorkProductId};
use rustc_middle::ty::TyCtxt;
use rustc_serialize::opaque::{FileEncodeResult, FileEncoder};
use rustc_serialize::Encodable as RustcEncodable;
use rustc_session::Session;
use std::fs;
use std::io;
use std::path::PathBuf;

use super::data::*;
use super::dirty_clean;
use super::file_format;
use super::fs::*;
use super::work_product;

/// Save and dump the DepGraph.
///
/// No query must be invoked after this function.
pub fn save_dep_graph(tcx: TyCtxt<'_>) {
    debug!("save_dep_graph()");
    tcx.dep_graph.with_ignore(|| {
        let sess = tcx.sess;
        if sess.opts.incremental.is_none() {
            return;
        }
        // This is going to be deleted in finalize_session_directory, so let's not create it
        if sess.has_errors_or_delayed_span_bugs() {
            return;
        }

        let query_cache_path = query_cache_path(sess);
        let dep_graph_path = dep_graph_path(sess);
        let staging_dep_graph_path = staging_dep_graph_path(sess);

        sess.time("assert_dep_graph", || crate::assert_dep_graph(tcx));
        sess.time("check_dirty_clean", || dirty_clean::check_dirty_clean_annotations(tcx));

        if sess.opts.debugging_opts.incremental_info {
            tcx.dep_graph.print_incremental_info()
        }

        join(
            move || {
                sess.time("incr_comp_persist_result_cache", || {
                    save_in(sess, query_cache_path, "query cache", |e| encode_query_cache(tcx, e));
                });
            },
            move || {
                sess.time("incr_comp_persist_dep_graph", || {
                    if let Err(err) = tcx.dep_graph.encode(&tcx.sess.prof) {
                        sess.err(&format!(
                            "failed to write dependency graph to `{}`: {}",
                            staging_dep_graph_path.display(),
                            err
                        ));
                    }
                    if let Err(err) = fs::rename(&staging_dep_graph_path, &dep_graph_path) {
                        sess.err(&format!(
                            "failed to move dependency graph from `{}` to `{}`: {}",
                            staging_dep_graph_path.display(),
                            dep_graph_path.display(),
                            err
                        ));
                    }
                });
            },
        );
    })
}

pub fn save_work_product_index(
    sess: &Session,
    dep_graph: &DepGraph,
    new_work_products: FxHashMap<WorkProductId, WorkProduct>,
) {
    if sess.opts.incremental.is_none() {
        return;
    }
    // This is going to be deleted in finalize_session_directory, so let's not create it
    if sess.has_errors_or_delayed_span_bugs() {
        return;
    }

    debug!("save_work_product_index()");
    dep_graph.assert_ignored();
    let path = work_products_path(sess);
    save_in(sess, path, "work product index", |e| encode_work_product_index(&new_work_products, e));

    // We also need to clean out old work-products, as not all of them are
    // deleted during invalidation. Some object files don't change their
    // content, they are just not needed anymore.
    let previous_work_products = dep_graph.previous_work_products();
    for (id, wp) in previous_work_products.iter() {
        if !new_work_products.contains_key(id) {
            work_product::delete_workproduct_files(sess, wp);
            debug_assert!(
                wp.saved_file.as_ref().map_or(true, |file_name| {
                    !in_incr_comp_dir_sess(sess, &file_name).exists()
                })
            );
        }
    }

    // Check that we did not delete one of the current work-products:
    debug_assert!({
        new_work_products
            .iter()
            .flat_map(|(_, wp)| wp.saved_file.iter())
            .map(|name| in_incr_comp_dir_sess(sess, name))
            .all(|path| path.exists())
    });
}

pub(crate) fn save_in<F>(sess: &Session, path_buf: PathBuf, name: &str, encode: F)
where
    F: FnOnce(&mut FileEncoder) -> FileEncodeResult,
{
    debug!("save: storing data in {}", path_buf.display());

    // Delete the old file, if any.
    // Note: It's important that we actually delete the old file and not just
    // truncate and overwrite it, since it might be a shared hard-link, the
    // underlying data of which we don't want to modify
    match fs::remove_file(&path_buf) {
        Ok(()) => {
            debug!("save: remove old file");
        }
        Err(err) if err.kind() == io::ErrorKind::NotFound => (),
        Err(err) => {
            sess.err(&format!(
                "unable to delete old {} at `{}`: {}",
                name,
                path_buf.display(),
                err
            ));
            return;
        }
    }

    let mut encoder = match FileEncoder::new(&path_buf) {
        Ok(encoder) => encoder,
        Err(err) => {
            sess.err(&format!("failed to create {} at `{}`: {}", name, path_buf.display(), err));
            return;
        }
    };

    if let Err(err) = file_format::write_file_header(&mut encoder, sess.is_nightly_build()) {
        sess.err(&format!("failed to write {} header to `{}`: {}", name, path_buf.display(), err));
        return;
    }

    if let Err(err) = encode(&mut encoder) {
        sess.err(&format!("failed to write {} to `{}`: {}", name, path_buf.display(), err));
        return;
    }

    if let Err(err) = encoder.flush() {
        sess.err(&format!("failed to flush {} to `{}`: {}", name, path_buf.display(), err));
        return;
    }

    debug!("save: data written to disk successfully");
}

fn encode_work_product_index(
    work_products: &FxHashMap<WorkProductId, WorkProduct>,
    encoder: &mut FileEncoder,
) -> FileEncodeResult {
    let serialized_products: Vec<_> = work_products
        .iter()
        .map(|(id, work_product)| SerializedWorkProduct {
            id: *id,
            work_product: work_product.clone(),
        })
        .collect();

    serialized_products.encode(encoder)
}

fn encode_query_cache(tcx: TyCtxt<'_>, encoder: &mut FileEncoder) -> FileEncodeResult {
    tcx.sess.time("incr_comp_serialize_result_cache", || tcx.serialize_query_result_cache(encoder))
}

pub fn build_dep_graph(
    sess: &Session,
    prev_graph: PreviousDepGraph,
    prev_work_products: FxHashMap<WorkProductId, WorkProduct>,
) -> Option<DepGraph> {
    if sess.opts.incremental.is_none() {
        // No incremental compilation.
        return None;
    }

    // Stream the dep-graph to an alternate file, to avoid overwriting anything in case of errors.
    let path_buf = staging_dep_graph_path(sess);

    let mut encoder = match FileEncoder::new(&path_buf) {
        Ok(encoder) => encoder,
        Err(err) => {
            sess.err(&format!(
                "failed to create dependency graph at `{}`: {}",
                path_buf.display(),
                err
            ));
            return None;
        }
    };

    if let Err(err) = file_format::write_file_header(&mut encoder, sess.is_nightly_build()) {
        sess.err(&format!(
            "failed to write dependency graph header to `{}`: {}",
            path_buf.display(),
            err
        ));
        return None;
    }

    // First encode the commandline arguments hash
    if let Err(err) = sess.opts.dep_tracking_hash().encode(&mut encoder) {
        sess.err(&format!(
            "failed to write dependency graph hash `{}`: {}",
            path_buf.display(),
            err
        ));
        return None;
    }

    Some(DepGraph::new(
        prev_graph,
        prev_work_products,
        encoder,
        sess.opts.debugging_opts.query_dep_graph,
        sess.opts.debugging_opts.incremental_info,
    ))
}
