use std::sync::{Arc, atomic::{AtomicBool, Ordering}};
use tokio::sync::{mpsc, oneshot};
use futures::Stream;
use std::pin::Pin;
use std::task::{Context, Poll};

// === Producer / Consumer (from split) ===

pub struct EventStreamProducer<T, R> {
    sender: mpsc::Sender<T>,
    result_sender: Option<oneshot::Sender<R>>,
    is_done: Arc<AtomicBool>,
}

impl<T: Send + 'static, R: Send + 'static> EventStreamProducer<T, R> {
    pub async fn push(&self, event: T) -> Result<(), mpsc::error::SendError<T>> {
        if self.is_done.load(Ordering::Relaxed) {
            return Ok(());
        }
        self.sender.send(event).await
    }

    pub fn end(&mut self, result: Option<R>) {
        self.is_done.store(true, Ordering::Relaxed);
        if let Some(sender) = self.result_sender.take() {
            if let Some(res) = result {
                let _ = sender.send(res);
            }
        }
    }
}

pub struct EventStreamConsumer<T, R> {
    receiver: mpsc::Receiver<T>,
    result_receiver: Option<oneshot::Receiver<R>>,
}

impl<T: Send + 'static, R: Send + 'static> EventStreamConsumer<T, R> {
    pub async fn next(&mut self) -> Option<T> {
        self.receiver.recv().await
    }

    pub async fn result(mut self) -> Option<R> {
        if let Some(rx) = self.result_receiver.take() {
            rx.await.ok()
        } else {
            None
        }
    }
}

impl<T: Send + 'static, R: Send + 'static> Stream for EventStreamConsumer<T, R> {
    type Item = T;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        self.receiver.poll_recv(cx)
    }
}

/// Generic event stream for async iteration and result handling
pub struct EventStream<T, R> {
    pub(crate) sender: Option<mpsc::Sender<T>>,
    receiver: Arc<std::sync::Mutex<Option<mpsc::Receiver<T>>>>,
    final_result_receiver: oneshot::Receiver<R>,
    pub(crate) final_result_sender: Option<oneshot::Sender<R>>,
    pub(crate) is_done: Arc<AtomicBool>,
}

impl<T, R> Default for EventStream<T, R>
where
    T: Send + 'static,
    R: Send + 'static,
{
    fn default() -> Self {
        Self::new()
    }
}

impl<T, R> EventStream<T, R>
where
    T: Send + 'static,
    R: Send + 'static,
{
    /// Create a new EventStream
    pub fn new() -> Self {
        let (tx, rx) = mpsc::channel(32);
        let (result_tx, result_rx) = oneshot::channel();

        Self {
            sender: Some(tx),
            receiver: Arc::new(std::sync::Mutex::new(Some(rx))),
            final_result_receiver: result_rx,
            final_result_sender: Some(result_tx),
            is_done: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Push an event to the stream
    pub async fn push(&self, event: T) -> Result<(), mpsc::error::SendError<T>> {
        if self.is_done.load(Ordering::Relaxed) {
            return Ok(()); // Ignore pushes after done
        }
        if let Some(ref sender) = self.sender {
            sender.send(event).await
        } else {
            Ok(())
        }
    }

    /// End the stream with an optional final result
    pub fn end(&mut self, result: Option<R>) {
        self.is_done.store(true, Ordering::Relaxed);
        if let Some(sender) = self.final_result_sender.take() {
            if let Some(res) = result {
                let _ = sender.send(res);
            }
        }
        // Drop the sender to close the channel
        self.sender.take();
    }

    /// Get the next event from the stream
    pub async fn next(&mut self) -> Option<T> {
        let mut rx = {
            let mut guard = self.receiver.lock().unwrap();
            guard.take()
        };
        let result = if let Some(ref mut receiver) = rx {
            receiver.recv().await
        } else {
            None
        };
        if let Some(receiver) = rx {
            let mut guard = self.receiver.lock().unwrap();
            *guard = Some(receiver);
        }
        result
    }

    /// Get the final result when the stream ends
    pub async fn result(self) -> Result<R, oneshot::error::RecvError> {
        self.final_result_receiver.await
    }

    /// Split into a producer/consumer pair. Consumes the EventStream.
    pub fn split(self) -> (EventStreamProducer<T, R>, EventStreamConsumer<T, R>) {
        let receiver = {
            let mut guard = self.receiver.lock().unwrap();
            guard.take().expect("EventStream receiver already taken")
        };

        let producer = EventStreamProducer {
            sender: self.sender.unwrap_or_else(|| mpsc::channel(1).0),
            result_sender: self.final_result_sender,
            is_done: self.is_done,
        };

        let consumer = EventStreamConsumer {
            receiver,
            result_receiver: Some(self.final_result_receiver),
        };

        (producer, consumer)
    }
}

impl<T, R> Stream for EventStream<T, R>
where
    T: Send + 'static,
    R: Send + 'static,
{
    type Item = T;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let mut receiver_guard = self.receiver.lock().unwrap();
        if let Some(ref mut rx) = *receiver_guard {
            match rx.poll_recv(cx) {
                Poll::Ready(Some(item)) => Poll::Ready(Some(item)),
                Poll::Ready(None) => Poll::Ready(None),
                Poll::Pending => Poll::Pending,
            }
        } else {
            Poll::Ready(None)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::StreamExt;
    use tokio::test;

    #[test]
    async fn test_event_stream_collect() {
        let mut stream = EventStream::<i32, String>::new();

        // Push some events
        stream.push(1).await.unwrap();
        stream.push(2).await.unwrap();
        stream.push(3).await.unwrap();

        // End the stream
        stream.end(Some("done".to_string()));

        // Collect events
        let events: Vec<_> = stream.collect().await;
        assert_eq!(events, vec![1, 2, 3]);
    }

    #[test]
    async fn test_event_stream_result() {
        let mut stream = EventStream::<i32, String>::new();

        // End the stream
        stream.end(Some("done".to_string()));

        // Get result
        let result = stream.result().await.unwrap();
        assert_eq!(result, "done");
    }

    #[test]
    async fn test_event_stream_next() {
        let mut stream = EventStream::<i32, ()>::new();

        stream.push(42).await.unwrap();

        let event = stream.next().await;
        assert_eq!(event, Some(42));
    }

    #[test]
    async fn test_split_producer_consumer() {
        let stream = EventStream::<i32, String>::new();
        let (producer, mut consumer) = stream.split();

        // Push from producer, read from consumer
        producer.push(1).await.unwrap();
        producer.push(2).await.unwrap();
        producer.push(3).await.unwrap();

        assert_eq!(consumer.next().await, Some(1));
        assert_eq!(consumer.next().await, Some(2));
        assert_eq!(consumer.next().await, Some(3));
    }

    #[test]
    async fn test_split_result() {
        let stream = EventStream::<i32, String>::new();
        let (mut producer, consumer) = stream.split();

        producer.end(Some("final".to_string()));

        let result = consumer.result().await;
        assert_eq!(result, Some("final".to_string()));
    }

    #[test]
    async fn test_split_consumer_stream_trait() {
        let stream = EventStream::<i32, String>::new();
        let (mut producer, consumer) = stream.split();

        // Spawn producer
        tokio::spawn(async move {
            producer.push(10).await.unwrap();
            producer.push(20).await.unwrap();
            producer.end(Some("done".to_string()));
        });

        // Collect via Stream trait
        let events: Vec<_> = consumer.collect().await;
        assert_eq!(events, vec![10, 20]);
    }
}