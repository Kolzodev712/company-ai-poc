use std::future::Future;

use crate::error::{Error, Result};

pub fn block_on<T>(future: impl Future<Output = T>) -> Result<T> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| {
            Error::app(
                "setup",
                format!("Could not start the local async runtime: {error}"),
            )
        })?;
    Ok(runtime.block_on(future))
}
