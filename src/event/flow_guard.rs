use ahash::HashMap;
use kovi::{
    event::Event,
    tokio::{self, sync::broadcast},
};
use std::{
    any::Any,
    sync::{Arc, Mutex},
};

pub enum WaitError {
    Timeout,
    Broadcast(broadcast::error::RecvError),
}

/// 流程守卫，用于确保事件按顺序执行
///
/// 使用 `FlowGuard::wait` 等待特定通知，会检查历史记录和未来通知
///
/// 使用 `FlowGuard::send` 发送通知
pub struct FlowGuard<T: Event> {
    /// 历史通知
    history: Mutex<HashMap<Arc<String>, Option<Arc<Context>>>>,
    /// 广播通知
    sender: broadcast::Sender<(Arc<String>, Option<Arc<Context>>)>,
    pub value: T,
}

impl<T: Event> Event for FlowGuard<T> {
    fn de(
        event: &kovi::event::InternalEvent,
        bot_info: &kovi::bot::BotInformation,
        api_tx: &kovi::tokio::sync::mpsc::Sender<kovi::types::ApiAndOneshot>,
    ) -> Option<Self>
    where
        Self: Sized,
    {
        let value = T::de(event, bot_info, api_tx)?;
        let (sender, _) = broadcast::channel(255); // 创建通知通道

        Some(Self {
            history: Mutex::new(HashMap::default()),
            sender,
            value,
        })
    }
}

impl<T: Event> FlowGuard<T> {
    /// 等待特定通知, 会检查历史记录和未来通知
    pub async fn wait_with_timeout(
        &self,
        notice: impl AsRef<str>,
        timeout: tokio::time::Duration,
    ) -> Result<Option<Arc<Context>>, WaitError> {
        let timeout_fut = tokio::time::sleep(timeout);

        tokio::pin!(timeout_fut);

        tokio::select! {
            biased;

            result = self.wait(notice) => {
                result.map_err(|err| {WaitError::Broadcast(err)})
            }

            _ = &mut timeout_fut => {
                return Err(WaitError::Timeout);
            }
        }
    }

    /// 等待特定通知, 会检查历史记录和未来通知
    pub async fn wait(
        &self,
        notice: impl AsRef<str>,
    ) -> Result<Option<Arc<Context>>, broadcast::error::RecvError> {
        let notice = notice.as_ref().to_string();

        // 在检查检查历史记录之前订阅通知通道
        let mut receiver = self.sender.subscribe();

        // 检查历史记录
        {
            let history = self.history.lock().unwrap();
            if let Some(context) = history.get(&notice) {
                return Ok(context.clone());
            }
        }

        loop {
            match receiver.recv().await {
                Ok((msg, ctx)) if *msg == notice => return Ok(ctx), // 匹配到目标通知
                Ok(_) => continue,                                  // 其他通知继续等待
                Err(err) => return Err(err),
            }
        }
    }

    /// 发送通知
    pub(crate) fn send_inner<V: Any + Send + Sync>(
        &self,
        notice: impl AsRef<str>,
        value: Option<V>,
    ) {
        let notice = notice.as_ref().to_string();
        let notice = Arc::new(notice);

        let arc_value = match value {
            Some(value) => Some(Arc::new(Context {
                any: Box::new(value),
            })),
            None => None,
        };

        let mut history = self.history.lock().unwrap();
        history.insert(notice.clone(), arc_value.clone());
        let _ = self.sender.send((notice, arc_value));
    }

    /// 发送无值通知
    pub fn send(&self, notice: impl AsRef<str>) {
        let none: Option<()> = None;
        self.send_inner(notice, none);
    }

    /// 发送带值通知
    pub fn send_value<V: Any + Send + Sync>(&self, notice: impl AsRef<str>, value: V) {
        self.send_inner(notice, Some(value));
    }
}

#[derive(Debug)]
pub struct Context {
    any: Box<dyn Any + Send + Sync>,
}
impl Context {
    /// 从上下文中获取一个值
    pub fn get<T: 'static>(&self) -> Option<&T> {
        let boxed = &self.any;
        (&**boxed as &(dyn Any + 'static)).downcast_ref()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kovi::event::Event;
    use std::time::Duration;

    // 创建一个简单的测试事件类型
    #[allow(dead_code)]
    #[derive(Debug, Clone)]
    struct TestEvent {
        id: u64,
    }

    impl Event for TestEvent {
        fn de(
            _event: &kovi::event::InternalEvent,
            _bot_info: &kovi::bot::BotInformation,
            _api_tx: &kovi::tokio::sync::mpsc::Sender<kovi::types::ApiAndOneshot>,
        ) -> Option<Self>
        where
            Self: Sized,
        {
            Some(TestEvent { id: 1 })
        }
    }

    // 创建一个 FlowGuard 实例用于测试
    fn create_test_flow_guard() -> FlowGuard<TestEvent> {
        let (sender, _) = broadcast::channel(255);
        FlowGuard {
            history: Mutex::new(HashMap::default()),
            sender,
            value: TestEvent { id: 1 },
        }
    }

    #[tokio::test]
    async fn test_send_and_wait_basic() {
        let guard = create_test_flow_guard();
        let notice = "test_notice";

        // 在另一个任务中发送通知
        let guard_clone = Arc::new(guard);
        let guard_for_send = guard_clone.clone();
        let notice_clone = notice.to_string();

        tokio::spawn(async move {
            // 稍微延迟一下，确保 wait 先开始
            tokio::time::sleep(Duration::from_millis(10)).await;
            guard_for_send.send(notice_clone);
        });

        // 等待通知
        let result = guard_clone.wait(notice).await;
        assert!(result.is_ok());
        assert!(result.unwrap().is_none());
    }

    #[tokio::test]
    async fn test_send_with_context() {
        let guard = create_test_flow_guard();
        let notice = "test_notice_with_context";
        let test_value = 42i32;

        // 发送带有上下文的通知
        guard.send_value(notice, test_value);

        // 等待通知
        let result = guard.wait(notice).await;
        assert!(result.is_ok());

        let context = result.unwrap();
        assert!(context.is_some());

        let ctx = context.unwrap();
        let retrieved_value = ctx.get::<i32>();
        assert!(retrieved_value.is_some());
        assert_eq!(*retrieved_value.unwrap(), 42i32);
    }

    #[tokio::test]
    async fn test_wait_from_history() {
        let guard = create_test_flow_guard();
        let notice = "historical_notice";

        // 先发送通知
        guard.send(notice);

        // 然后等待，应该立即从历史记录中获取
        let result = guard.wait(notice).await;
        assert!(result.is_ok());
        assert!(result.unwrap().is_none());
    }

    #[tokio::test]
    async fn test_wait_with_timeout_success() {
        let guard = Arc::new(create_test_flow_guard());
        let notice = "timeout_test_success";
        let timeout = Duration::from_millis(100);

        let guard_for_send = guard.clone();
        let notice_clone = notice.to_string();

        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(50)).await;
            guard_for_send.send(notice_clone);
        });

        let result = guard.wait_with_timeout(notice, timeout).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_wait_with_timeout_failure() {
        let guard = create_test_flow_guard();
        let notice = "timeout_test_failure";
        let timeout = Duration::from_millis(50);

        let result = guard.wait_with_timeout(notice, timeout).await;
        assert!(result.is_err());

        match result.unwrap_err() {
            WaitError::Timeout => {} // 这是我们期望的
            _ => panic!("Expected timeout error"),
        }
    }

    #[tokio::test]
    async fn test_multiple_waiters_same_notice() {
        let guard = Arc::new(create_test_flow_guard());
        let notice = "multi_waiter_test";

        // 创建多个等待者
        let mut handles = vec![];
        for i in 0..5 {
            let guard_clone = guard.clone();
            let notice_clone = notice.to_string();
            let handle = tokio::spawn(async move {
                let result = guard_clone.wait(notice_clone).await;
                (i, result.is_ok())
            });
            handles.push(handle);
        }

        // 稍微延迟后发送通知
        tokio::time::sleep(Duration::from_millis(10)).await;
        guard.send(notice);

        // 等待所有任务完成
        let mut success_count = 0;
        for handle in handles {
            let (_, success) = handle.await.unwrap();
            if success {
                success_count += 1;
            }
        }

        // 所有等待者都应该成功
        assert_eq!(success_count, 5);
    }

    #[tokio::test]
    async fn test_different_notices() {
        let guard = Arc::new(create_test_flow_guard());

        // 发送不同的通知
        guard.send_value("notice1", "value1".to_string());
        guard.send_value("notice2", 42i32);
        guard.send("notice3");

        // 等待不同的通知
        let result1 = guard.wait("notice1").await;
        assert!(result1.is_ok());
        let ctx1 = result1.unwrap().unwrap();
        assert_eq!(ctx1.get::<String>().unwrap(), "value1");

        let result2 = guard.wait("notice2").await;
        assert!(result2.is_ok());
        let ctx2 = result2.unwrap().unwrap();
        assert_eq!(*ctx2.get::<i32>().unwrap(), 42);

        let result3 = guard.wait("notice3").await;
        assert!(result3.is_ok());
        assert!(result3.unwrap().is_none());
    }

    #[tokio::test]
    async fn test_context_type_safety() {
        let guard = create_test_flow_guard();
        let notice = "type_safety_test";

        // 发送一个字符串值
        guard.send_value(notice, "hello".to_string());

        let result = guard.wait(notice).await;
        assert!(result.is_ok());

        let context = result.unwrap().unwrap();

        // 尝试获取正确的类型
        let string_val = context.get::<String>();
        assert!(string_val.is_some());
        assert_eq!(string_val.unwrap(), "hello");

        // 尝试获取错误的类型
        let int_val = context.get::<i32>();
        assert!(int_val.is_none());
    }

    #[tokio::test]
    async fn test_concurrent_send_and_wait() {
        let guard = Arc::new(create_test_flow_guard());
        let mut sender_handles = vec![];
        let mut waiter_handles = vec![];

        // 创建多个发送者和等待者
        for i in 0..10 {
            let guard_clone = guard.clone();
            let notice = format!("concurrent_test_{}", i);

            // 发送者
            let sender_guard = guard_clone.clone();
            let sender_notice = notice.clone();
            sender_handles.push(tokio::spawn(async move {
                tokio::time::sleep(Duration::from_millis(i * 10)).await;
                sender_guard.send_value(sender_notice, i);
            }));

            // 等待者
            waiter_handles.push(tokio::spawn(async move {
                let result = guard_clone.wait(notice).await;
                result.is_ok()
            }));
        }

        // 等待所有发送者完成
        for handle in sender_handles {
            let _ = handle.await;
        }

        // 等待所有等待者完成
        let mut success_count = 0;
        for handle in waiter_handles {
            match handle.await {
                Ok(true) => success_count += 1,
                Ok(false) => {}
                Err(_) => {}
            }
        }

        // 所有的等待者都应该成功
        assert_eq!(success_count, 10);
    }

    #[tokio::test]
    async fn test_wait_nonexistent_notice_with_timeout() {
        let guard = create_test_flow_guard();
        let result = guard
            .wait_with_timeout("nonexistent", Duration::from_millis(10))
            .await;

        assert!(result.is_err());
        match result.unwrap_err() {
            WaitError::Timeout => {}
            _ => panic!("Expected timeout error"),
        }
    }
}
