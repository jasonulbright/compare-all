//! Runtime abstraction over tokio (native) and wasm-bindgen-futures / gloo-timers (wasm32).

use std::{
    fmt,
    future::Future,
    pin::Pin,
    task::{Context, Poll},
    time::Duration,
};

use super::error::Error;

#[derive(Debug)]
pub struct JoinError;

impl fmt::Display for JoinError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("JoinError")
    }
}

impl std::error::Error for JoinError {}

pub struct JoinHandle<T: Send> {
    rx: tokio::sync::oneshot::Receiver<T>,
}

impl<T: Send> Future for JoinHandle<T> {
    type Output = Result<T, JoinError>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        match Pin::new(&mut self.rx).poll(cx) {
            Poll::Ready(Ok(v)) => Poll::Ready(Ok(v)),
            Poll::Ready(Err(_)) => Poll::Ready(Err(JoinError)),
            Poll::Pending => Poll::Pending,
        }
    }
}

pub fn spawn<F, T>(future: F) -> JoinHandle<T>
where
    F: Future<Output = T> + Send + 'static,
    T: Send + 'static,
{
    let (tx, rx) = tokio::sync::oneshot::channel();
    tokio::spawn(async move {
        let _ = tx.send(future.await);
    });
    JoinHandle { rx }
}

pub async fn timeout<F: Future>(duration: Duration, future: F) -> Result<F::Output, Error> {
    tokio::time::timeout(duration, future)
        .await
        .map_err(|_| Error::Timeout)
}
