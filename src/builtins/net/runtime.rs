use std::future::Future;
use std::sync::LazyLock;
use tokio::runtime::{Builder, Runtime};

/// Network commands use a single runtime for the lifetime of the process.
///
/// Network builtins are process-isolated, so this runtime owns every task and
/// socket for exactly one command invocation.
static RUNTIME: LazyLock<Runtime> = LazyLock::new(|| {
    Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("network runtime must be constructible")
});

/// Run one network command future to completion.
pub fn block_on<F: Future>(future: F) -> F::Output {
    RUNTIME.block_on(future)
}
