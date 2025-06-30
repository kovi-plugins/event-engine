# Kovi 的 事件驱动器

提供更多kovi框架的事件

目前有

### FlowGuard

用于流程管控，可以等待其他的接受相同事件的监听闭包的通知

```rust
#[kovi::plugin]
async fn main() {
    P::on(move |e: Arc<FlowGuard<MsgEvent>>| async move {
        e.send("notice");

        e.wait("notice").await.unwrap();

        let msg_event = &e.value;

        msg_event.reply("msg");
    })
}
```

也可以用于发送接收传过来的值

```rust
#[kovi::plugin]
async fn main() {
    P::on(move |e: Arc<FlowGuard<MsgEvent>>| async move {
        e.send_value("notice", String::from("我是一个值"));

        let context = e.wait("notice").await.unwrap();

        let msg = context.get<String>().unwrap;

        assert_eq!(msg, "我是一个值");

        let msg_event = &e.value;

        msg_event.reply(msg);
    })
}
```
