use rustc_middle::ty::TyCtxt;
use rustc_span::{edition::Edition, Symbol};

use crate::clean;
use crate::config::RenderOptions;
use crate::error::Error;
use crate::formats::cache::Cache;

/// Allows for different backends to rustdoc to be used with the `run_format()` function. Each
/// backend renderer has hooks for initialization, documenting an item, entering and exiting a
/// module, and cleanup/finalizing output.
crate trait FormatRenderer<'tcx>: Sized {
    /// Gives a description of the renderer. Used for performance profiling.
    fn descr() -> &'static str;

    /// Whether to call `item` recursivly for modules
    ///
    /// This is true for html, and false for json. See #80664
    const RUN_ON_MODULE: bool;

    /// Sets up any state required for the renderer. When this is called the cache has already been
    /// populated.
    fn init(
        krate: clean::Crate,
        options: RenderOptions,
        edition: Edition,
        cache: Cache,
        tcx: TyCtxt<'tcx>,
    ) -> Result<(Self, clean::Crate), Error>;

    /// Make a new renderer to render a child of the item currently being rendered.
    fn make_child_renderer(&self) -> Self;

    /// Renders a single non-module item. This means no recursive sub-item rendering is required.
    fn item(&mut self, item: clean::Item) -> Result<(), Error>;

    /// Renders a module (should not handle recursing into children).
    fn mod_item_in(&mut self, item: &clean::Item, item_name: &str) -> Result<(), Error>;

    /// Runs after recursively rendering all sub-items of a module.
    fn mod_item_out(&mut self, item_name: &str) -> Result<(), Error>;

    /// Post processing hook for cleanup and dumping output to files.
    ///
    /// A handler is available if the renderer wants to report errors.
    fn after_krate(
        &mut self,
        crate_name: Symbol,
        diag: &rustc_errors::Handler,
    ) -> Result<(), Error>;

    fn cache(&self) -> &Cache;
}

/// Main method for rendering a crate.
crate fn run_format<'tcx, T: FormatRenderer<'tcx>>(
    krate: clean::Crate,
    options: RenderOptions,
    cache: Cache,
    diag: &rustc_errors::Handler,
    edition: Edition,
    tcx: TyCtxt<'tcx>,
) -> Result<(), Error> {
    let prof = &tcx.sess.prof;

    let emit_crate = options.should_emit_crate();
    let (mut format_renderer, krate) = prof
        .extra_verbose_generic_activity("create_renderer", T::descr())
        .run(|| T::init(krate, options, edition, cache, tcx))?;

    if !emit_crate {
        return Ok(());
    }

    // Render the crate documentation
    let crate_name = krate.name;
    let mut work = vec![(format_renderer.make_child_renderer(), krate.module)];

    let unknown = Symbol::intern("<unknown item>");
    while let Some((mut cx, item)) = work.pop() {
        if item.is_mod() && T::RUN_ON_MODULE {
            // modules are special because they add a namespace. We also need to
            // recurse into the items of the module as well.
            let name = item.name.as_ref().unwrap().to_string();
            if name.is_empty() {
                panic!("Unexpected module with empty name");
            }
            let _timer = prof.generic_activity_with_arg("render_mod_item", name.as_str());

            cx.mod_item_in(&item, &name)?;
            let module = match *item.kind {
                clean::StrippedItem(box clean::ModuleItem(m)) | clean::ModuleItem(m) => m,
                _ => unreachable!(),
            };
            for it in module.items {
                debug!("Adding {:?} to worklist", it.name);
                work.push((cx.make_child_renderer(), it));
            }

            cx.mod_item_out(&name)?;
        // FIXME: checking `item.name.is_some()` is very implicit and leads to lots of special
        // cases. Use an explicit match instead.
        } else if item.name.is_some() && !item.is_extern_crate() {
            prof.generic_activity_with_arg("render_item", &*item.name.unwrap_or(unknown).as_str())
                .run(|| cx.item(item))?;
        }
    }
    prof.extra_verbose_generic_activity("renderer_after_krate", T::descr())
        .run(|| format_renderer.after_krate(crate_name, diag))
}
