//! LCEL — the LangChain Expression Language, rebuilt in Rust.
//!
//! Python LCEL composes runnables with `|` (pipe). Here the same idea is expressed
//! with the [`Runnable`] trait plus the [`RunnableExt::pipe`] combinator, which
//! type-checks the intermediate value at compile time instead of runtime:
//!
//! ```text
//! prompt | model | parser      (Python)
//! prompt.pipe(model).pipe(parser)   (this crate)
//! ```

use async_trait::async_trait;
use futures::stream::{self, StreamExt};
use std::sync::Arc;

use crate::errors::{LangchainError, Result};

/// Anything that can transform an input of type `I` into an output of type `O`.
///
/// This is `langchain_core.runnables.Runnable` in Rust: `invoke` is the primary
/// path, `batch` runs many inputs concurrently, `stream` yields partial outputs.
#[async_trait]
pub trait Runnable<I: Send + 'static, O: Send + 'static>: Send + Sync + Debug {
    /// Run once with `input`.
    async fn invoke(&self, input: I) -> Result<O>;

    /// Run concurrently over `inputs`, preserving order (like `Runnable.batch`).
    async fn batch(&self, inputs: Vec<I>) -> Result<Vec<O>> {
        let futs: Vec<_> = inputs.into_iter().map(|i| self.invoke(i)).collect();
        stream::iter(futs).buffered(16).collect::<Vec<_>>().await.into_iter().collect()
    }
}

// Debug is required on Runnable implementors so that derived Debug on containers works.
use std::fmt::Debug;

/// A `Runnable` implemented by any async closure — `RunnableLambda` in Python LCEL.
pub struct RunnableLambda<I, O, F, Fut>
where
    F: Fn(I) -> Fut + Send + Sync,
    Fut: std::future::Future<Output = Result<O>> + Send,
{
    f: Arc<F>,
    _marker: std::marker::PhantomData<fn(I) -> (O, Fut)>,
}

impl<I, O, F, Fut> std::fmt::Debug for RunnableLambda<I, O, F, Fut>
where
    F: Fn(I) -> Fut + Send + Sync,
    Fut: std::future::Future<Output = Result<O>> + Send,
{
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("RunnableLambda")
    }
}

impl<I, O, F, Fut> RunnableLambda<I, O, F, Fut>
where
    F: Fn(I) -> Fut + Send + Sync,
    Fut: std::future::Future<Output = Result<O>> + Send,
{
    /// Wrap an async closure into a runnable.
    pub fn new(f: F) -> Self {
        Self { f: Arc::new(f), _marker: std::marker::PhantomData }
    }
}

#[async_trait]
impl<I, O, F, Fut> Runnable<I, O> for RunnableLambda<I, O, F, Fut>
where
    I: Send + 'static,
    O: Send + 'static,
    F: Fn(I) -> Fut + Send + Sync,
    Fut: std::future::Future<Output = Result<O>> + Send,
{
    async fn invoke(&self, input: I) -> Result<O> {
        (self.f)(input).await
    }
}

/// Convenience constructor matching Python's `RunnableLambda(fn)`.
pub fn lambda<I, O, F, Fut>(f: F) -> Arc<dyn Runnable<I, O>>
where
    I: Send + 'static,
    O: Send + 'static,
    F: Fn(I) -> Fut + Send + Sync + 'static,
    Fut: std::future::Future<Output = Result<O>> + Send + 'static,
{
    Arc::new(RunnableLambda::new(f))
}

/// Returns its input unchanged — `RunnablePassthrough`.
#[derive(Debug, Default, Clone, Copy)]
pub struct RunnablePassthrough;

#[async_trait]
impl<I: Send + 'static> Runnable<I, I> for RunnablePassthrough
where
    I: Clone,
{
    async fn invoke(&self, input: I) -> Result<I> {
        Ok(input)
    }
}

/// Runs several runnables on the **same** input concurrently and returns all
/// outputs in order — the parallel half of Python's `RunnableParallel`.
pub struct RunnableMap<I, O> {
    runnables: Vec<Arc<dyn Runnable<I, O>>>,
}

impl<I, O> std::fmt::Debug for RunnableMap<I, O> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "RunnableMap(n={})", self.runnables.len())
    }
}

impl<I: Send + Clone + 'static, O: Send + 'static> RunnableMap<I, O> {
    /// Empty map.
    pub fn new() -> Self {
        Self { runnables: Vec::new() }
    }

    /// Register another runnable that consumes the same input.
    pub fn with(mut self, r: Arc<dyn Runnable<I, O>>) -> Self {
        self.runnables.push(r);
        self
    }
}

impl<I: Send + Clone + 'static, O: Send + 'static> Default for RunnableMap<I, O> {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl<I: Send + Clone + 'static, O: Send + 'static> Runnable<I, Vec<O>> for RunnableMap<I, O> {
    async fn invoke(&self, input: I) -> Result<Vec<O>> {
        if self.runnables.is_empty() {
            return Err(LangchainError::Other("RunnableMap has no runnables".into()));
        }
        let input0 = input;
        let outs = stream::iter(self.runnables.iter())
            .then(move |r| {
                let inp = input0.clone();
                async move { r.invoke(inp).await }
            })
            .collect::<Vec<_>>()
            .await;
        outs.into_iter().collect()
    }
}

/// Sequential composition produced by [`RunnableExt::pipe`].
/// `RunnableSequence<A, B, I, M, O>: Runnable<I, O>` where `A: Runnable<I, M>`, `B: Runnable<M, O>`.
pub struct RunnableSequence<A, B, I, M, O> {
    pub first: A,
    pub then: B,
    _marker: std::marker::PhantomData<fn(I) -> (M, O)>,
}

impl<A, B, I, M, O> std::fmt::Debug for RunnableSequence<A, B, I, M, O> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("RunnableSequence")
    }
}

/// Composition helpers, the Rust `|` operator of LCEL.
pub trait RunnableExt<I: Send + 'static, O: Send + 'static>: Runnable<I, O> + Sized {
    /// Chain `self` with `next`: `self.invoke(x)` feeds `next.invoke`.
    ///
    /// ```ignore
    /// let chain = prompt.pipe(model).pipe(str_parser());
    /// let answer: String = chain.invoke(vars).await?;
    /// ```
    fn pipe<M: Send + 'static, N: Runnable<O, M>>(self, next: N) -> RunnableSequence<Self, N, I, O, M> {
        RunnableSequence { first: self, then: next, _marker: std::marker::PhantomData }
    }
}

impl<I: Send + 'static, O: Send + 'static, T: Runnable<I, O> + Sized> RunnableExt<I, O> for T {}

#[async_trait]
impl<I: Send + 'static, M: Send + 'static, O: Send + 'static, A, B> Runnable<I, O>
    for RunnableSequence<A, B, I, M, O>
where
    A: Runnable<I, M>,
    B: Runnable<M, O>,
{
    async fn invoke(&self, input: I) -> Result<O> {
        let mid = self.first.invoke(input).await?;
        self.then.invoke(mid).await
    }

    async fn batch(&self, inputs: Vec<I>) -> Result<Vec<O>> {
        let mids = self.first.batch(inputs).await?;
        self.then.batch(mids).await
    }
}

/// `Arc<dyn Runnable<I, O>>` is itself a `Runnable`, so boxed steps chain with
/// concrete ones interchangeably.
#[async_trait]
impl<I: Send + 'static, O: Send + 'static> Runnable<I, O> for Arc<dyn Runnable<I, O>> {
    async fn invoke(&self, input: I) -> Result<O> {
        self.as_ref().invoke(input).await
    }

    async fn batch(&self, inputs: Vec<I>) -> Result<Vec<O>> {
        self.as_ref().batch(inputs).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn double(x: i32) -> impl std::future::Future<Output = Result<i32>> {
        async move { Ok(x * 2) }
    }

    fn add_ten(x: i32) -> impl std::future::Future<Output = Result<i32>> {
        async move { Ok(x + 10) }
    }

    #[tokio::test]
    async fn pipe_composes() {
        let a = lambda(double);
        let b = lambda(add_ten);
        let chain = a.pipe(b); // (x*2) + 10
        assert_eq!(chain.invoke(5).await.unwrap(), 20);
        assert_eq!(chain.batch(vec![0, 1, 2]).await.unwrap(), vec![10, 12, 14]);
    }

    #[tokio::test]
    async fn passthrough_and_map() {
        let pt: RunnablePassthrough = RunnablePassthrough;
        assert_eq!(pt.invoke(42i64).await.unwrap(), 42);

        let map = RunnableMap::new()
            .with(lambda(double))
            .with(lambda(add_ten));
        let outs = map.invoke(3).await.unwrap();
        assert_eq!(outs, vec![6, 13]);
    }

    #[tokio::test]
    async fn three_stage_chain() {
        let chain = lambda(double)
            .pipe(lambda(add_ten))
            .pipe(lambda(|x: i32| async move { Ok(format!("result: {x}")) }));
        assert_eq!(chain.invoke(2).await.unwrap(), "result: 14");
    }
}
