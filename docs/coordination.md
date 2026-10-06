# Worker communication

Workers in one codemod share saved task mailboxes. They can ask questions, reply and send updates while keeping separate file ownership. Rust routes messages to the worker currently assigned to the recipient task.

```mermaid
sequenceDiagram
    participant A as Worker A · task A
    participant H as Rust harness · saved mailboxes
    participant B as Worker B · task B
    participant U as You · composer
    A->>H: Ask task B
    H->>B: Deliver into its active turn or next task start
    B->>H: Acknowledge receipt and reply
    H->>A: Deliver reply · resume waiting work
    opt A product decision needs your input
        A->>H: Ask you
        H->>U: Highlight question beside the answer composer
        U->>H: Enter · save answer
        H->>A: Deliver answer · resume waiting work
    end
```

## What you see

Routine messages stay behind **Ctrl+O**. Questions for you stay visible, with the sender and message ID. The composer changes to **answer #ID**; Enter answers that question instead of queueing an edit. If several workers ask, answer them in order. Your draft stays intact when a question arrives.

An executor can finish independent work before pausing for a reply. Unanswered asks prevent task completion. After its questions are answered, Rust resumes a waiting task in its existing folder. Other independent tasks keep running. Reopened projects wait for **Ctrl+R** to reconnect workers.

## Worker tools

| Tool | Purpose |
| --- | --- |
| `send_worker_message` | Send an `ask`, `reply` or `update` to a plan task ID; asks can target `user` |
| `read_worker_messages` | Read the assigned task's inbox, sent messages and peer assignments |
| `ack_worker_messages` | Confirm receipt of specific inbox message IDs |

Replies reference the original ask. Stable keys make repeated sends idempotent. Rust binds codemod and worker identity; model arguments cannot impersonate another worker. Messages carry context and cannot change file scope, permissions or the user's goal.

Messages are saved before delivery. **Delivered** means Codex accepted the injection; **acknowledged** means the recipient called the receipt tool; **answered** means a reply was saved. None means the source passed verification. Reconnect checks uncertain delivery against the saved conversation. Rejected injections stay in the inbox for a later read.

## Boundaries

Messages stay within one codemod. Task IDs follow current ownership, including tasks not yet assigned. Asks to unavailable tasks or asks that create a wait cycle are rejected. Workers read at useful checkpoints and continue independent work; they do not poll for replies.

Bodies are limited to 4,000 bytes, with up to 32 unacknowledged messages per task. SQLite retains messages as local plain text. New edit and integration rounds keep the history but use new task identities, so old asks cannot reopen. Closing retains mailboxes; deleting the codemod removes them.
