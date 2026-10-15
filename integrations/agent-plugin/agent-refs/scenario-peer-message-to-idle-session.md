# Scenario: a message to an idle peer

1. `legionctl thread peers` shows the peer `b71c…` with state `idle` and the channels `outbox, claude-inbox`.
2. Write the message into the outbox: `legionctl thread send b71c… note "W3 schema changed; rebase before you
   edit io.rs"`. The outbox keeps it after restarts.
3. An idle Claude Code peer gets the outbox message only at its next prompt. If it must act now, also send it with
   `SendMessage`. The post-tool hook records that call in the thread log.
4. The peer acks the message. `thread show` shows it `acked`.
