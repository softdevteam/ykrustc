// Copyright 2019 King's College London.
// Created by the Software Development Team <http://soft-dev.org/>.
//
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// http://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or http://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

#include <stdint.h>
#include <stdlib.h>
#include <err.h>
#include <stdbool.h>
#include <stdatomic.h>

struct mir_loc {
    uint64_t crate_hash;
    uint32_t def_idx;
    uint32_t bb_idx;
};

#define TL_TRACE_INIT_CAP       1024
#define TL_TRACE_REALLOC_CAP    1024

void yk_swt_start_tracing_impl(void);
void yk_swt_rec_loc_impl(uint64_t crate_hash, uint32_t def_idx, uint32_t bb_idx);
struct mir_loc *yk_swt_stop_tracing_impl(size_t *ret_trace_len);
void yk_swt_invalidate_trace_impl(void);

// The trace buffer.
static __thread struct mir_loc *trace_buf = NULL;
// The number of elements in the trace buffer.
static __thread size_t trace_buf_len = 0;
// The allocation capacity of the trace buffer (in elements).
static __thread size_t trace_buf_cap = 0;
// Is the current thread tracing?
// true = we are tracing, false = we are not tracing or an error occurred.
static __thread volatile atomic_bool tracing = false;

// Start tracing on the current thread.
// A new trace buffer is allocated and MIR locations will be written into it on
// subsequent calls to `yk_swt_rec_loc_impl`. If the current thread is already
// tracing, calling this will lead to undefined behaviour.
void
yk_swt_start_tracing_impl(void) {
    trace_buf = calloc(TL_TRACE_INIT_CAP, sizeof(struct mir_loc));
    if (trace_buf == NULL) {
        err(EXIT_FAILURE, "%s: calloc: ", __func__);
    }

    trace_buf_cap = TL_TRACE_INIT_CAP;
    atomic_store_explicit(&tracing, true, memory_order_relaxed);
}

// Record a location into the trace buffer if tracing is enabled on the current thread.
void
yk_swt_rec_loc_impl(uint64_t crate_hash, uint32_t def_idx, uint32_t bb_idx)
{
    if (!atomic_load_explicit(&tracing, memory_order_relaxed)) {
        return;
    }

    // Check if we need more space and reallocate if necessary.
    if (trace_buf_len == trace_buf_cap) {
        if (trace_buf_cap >= SIZE_MAX - TL_TRACE_REALLOC_CAP) {
            // Trace capacity would overflow.
            atomic_store_explicit(&tracing, false, memory_order_relaxed);
            return;
        }
        size_t new_cap = trace_buf_cap + TL_TRACE_REALLOC_CAP;

        if (new_cap > SIZE_MAX / sizeof(struct mir_loc)) {
            // New buffer size would overflow.
            atomic_store_explicit(&tracing, false, memory_order_relaxed);
            return;
        }
        size_t new_size = new_cap * sizeof(struct mir_loc);

        trace_buf = realloc(trace_buf, new_size);
        if (trace_buf == NULL) {
            atomic_store_explicit(&tracing, false, memory_order_relaxed);
            return;
        }

        trace_buf_cap = new_cap;
    }

    struct mir_loc loc = { crate_hash, def_idx, bb_idx };
    trace_buf[trace_buf_len] = loc;
    trace_buf_len ++;
}


// Stop tracing on the current thread.
// On success the trace buffer is returned and the number of locations it
// holds is written to `*ret_trace_len`. It is the responsibility of the caller
// to free the returned trace buffer. A NULL pointer is returned on error.
// Calling this function when tracing was not started with
// `yk_swt_start_tracing_impl()` results in undefined behaviour.
struct mir_loc *
yk_swt_stop_tracing_impl(size_t *ret_trace_len) {
    if (!atomic_load_explicit(&tracing, memory_order_relaxed)) {
        free(trace_buf);
        trace_buf = NULL;
        trace_buf_len = 0;
    }

    // We hand ownership of the trace to Rust now. Rust is responsible for
    // freeing the trace.
    struct mir_loc *ret_trace = trace_buf;
    *ret_trace_len = trace_buf_len;

    // Now reset all off the recorder's state.
    // We reset `trace_invalid` when tracing is restarted, because signals
    // handlers which set this flag may arrive in the meantime.
    trace_buf = NULL;
    tracing = false;
    trace_buf_len = 0;
    trace_buf_cap = 0;

    return ret_trace;
}

// Call this to safely mark the trace invalid.
void
yk_swt_invalidate_trace_impl(void) {
    // We don't free the trace buffer here, as this may be called in a signal
    // handler and thus needs to be reentrant.
    atomic_store_explicit(&tracing, false, memory_order_relaxed);
}
