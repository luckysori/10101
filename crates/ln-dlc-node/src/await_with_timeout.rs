use futures::future::BoxFuture;
use std::future::Future;
use std::time::Duration;

pub trait AwaitWithTimeout<'a>
where
    Self: Future,
{
    fn await_with_timeout(self)
        -> BoxFuture<'a, Result<Self::Output, tokio::time::error::Elapsed>>;
}

impl<'a, F, T> AwaitWithTimeout<'a> for F
where
    F: Future<Output = T> + Send + 'a,
{
    fn await_with_timeout(self) -> BoxFuture<'a, Result<T, tokio::time::error::Elapsed>> {
        Box::pin(async { tokio::time::timeout(Duration::from_secs(30), self).await })
    }
}
