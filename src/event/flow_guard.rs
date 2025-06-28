use ahash::HashSet;
use kovi::{
    event::Event,
    tokio::{self, sync::broadcast},
};
use std::sync::{Arc, Mutex};

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
    history: Mutex<HashSet<Arc<String>>>,
    /// 广播通知
    sender: broadcast::Sender<Arc<String>>,
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
            history: Mutex::new(HashSet::default()),
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
    ) -> Result<(), WaitError> {
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
    pub async fn wait(&self, notice: impl AsRef<str>) -> Result<(), broadcast::error::RecvError> {
        let notice = notice.as_ref().to_string();

        // 在检查检查历史记录之前订阅通知通道
        let mut receiver = self.sender.subscribe();

        // 检查历史记录
        {
            let history = self.history.lock().unwrap();
            if history.contains(&notice) {
                return Ok(());
            }
        }

        loop {
            match receiver.recv().await {
                Ok(msg) if *msg == notice => return Ok(()), // 匹配到目标通知
                Ok(_) => continue,                          // 其他通知继续等待
                Err(err) => return Err(err),
            }
        }
    }

    /// 发送通知
    pub async fn send(&self, notice: impl AsRef<str>) {
        let notice = notice.as_ref().to_string();

        let notice = Arc::new(notice);

        let mut history = self.history.lock().unwrap();
        history.insert(notice.clone());

        let _ = self.sender.send(notice);
    }
}
