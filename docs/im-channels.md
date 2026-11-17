# IM Chat Channels

Vibex can be reached from Telegram and Lark / 飞书. You pair your chat with one
runtime once; after that you can send prompts, answer approvals, and read final
answers without opening the desktop app.

The runtime stays the authority. An IM channel is a remote control: a message
that reaches it is a prompt to the same Agent session the desktop shows, and an
approval tapped in chat resolves the same `PermissionRequest` the desktop would
show you.

## What is supported

| | Telegram | Lark / 飞书 |
| --- | --- | --- |
| Direct message | yes | yes |
| Group chat | yes, commands and mentions | yes, commands |
| Approval buttons | inline keyboard | interactive card buttons |
| Approval by text | `/approve <code>` | `/approve <code>` |
| Images and documents in | yes, up to 10 MB | yes, up to 10 MB |
| Forum topics | one topic per `/new` | threads |
| Transport | long poll | long connection |
| Rich text | Markdown rendered as HTML | plain text |
| Live turn | progress line and streamed answer | progress line and streamed answer |
| Command menu | `/` picker via `setMyCommands` | `/` picker via the command list |

Enterprise WeChat and personal WeChat are not implemented and are not offered in
settings or on the command line.

## How a turn appears

A reply used to arrive only when the turn ended. It now appears while the turn
runs, in every chat and at every level:

- Each assistant text block is its own message, edited as that block streams. A
  turn that writes, calls a tool, and writes again therefore leaves several
  readable messages behind rather than one message that keeps changing.
- While tools run and the model reasons, one progress message is edited in
  place: `⏳ Thinking · 12s`, `⏳ Running a command · 30s`. The line names the
  kind of work, never its input or output. When the turn finishes it settles on
  `✅ Done`.

An answer too long for one chat message is split into `(1/3)`, `(2/3)`, …; the
first part replaces the streamed message and the rest arrive as new messages.

Edits are coalesced, so a burst of deltas costs one edit, and they wait behind a
provider rate limit rather than queueing ahead of approvals and final answers.
An interrupted turn leaves `⏹ Stopped.` behind instead of a message that never
finishes.

Markdown is rendered where the transport supports it: Telegram receives HTML
(`<b>`, `<code>`, `<pre>`, `<blockquote>`, links) generated from the answer. If
Telegram refuses the markup, the same text is re-sent as plain text, so a stray
`<` in a code sample costs the formatting and never the answer.

### Reactions

When the Agent reacts to a message — the desktop's message-reaction setting is
what lets it — the same emoji is set on the chat message that sent the prompt.
The channel knows which chat message a prompt came from because it remembers the
correlation the submission carried and matches it to the user item the runtime
publishes.

The mirror is close to the provider's vocabulary, not the Agent's: Telegram only
accepts its own list of reaction emoji, so a reaction outside it is skipped
rather than sent and refused. A transport without reactions, such as Lark text
messages, ignores them.

## Create the bot

### Telegram

1. Talk to [@BotFather](https://t.me/BotFather), send `/newbot`, and copy the
   token it returns.
2. Add the bot to the chat you want it to serve. For a group, give it the right
   to read messages, or it only sees commands.
3. Get the chat id: send the bot one message, then open
   `https://api.telegram.org/bot<token>/getUpdates` and read
   `message.chat.id`. Group ids are negative; supergroups start with `-100`.
4. In Vibex, open **Settings → IM channels**, fill in the name, token, and chat
   id, and choose whether `/new` should create one forum topic per session.
   That mode needs a forum supergroup and the bot as an administrator.
5. Press **Test connection**, then turn the channel on.

### Lark / 飞书

Lark has two deployments, and they are separate services with separate app
registrations:

| | China | International |
| --- | --- | --- |
| Console | [open.feishu.cn](https://open.feishu.cn) | [open.larksuite.com](https://open.larksuite.com) |
| API root | `https://open.feishu.cn` | `https://open.larksuite.com` |

**A 飞书 app and a Lark app are different apps.** An app id created on one does
not exist on the other; the token endpoint answers `10014 app id not exists`.
Pick the deployment in the channel form before testing, and create the app on
the matching console. If you are in China and using 飞书, choose 飞书 中国.

1. In the [Lark open platform](https://open.feishu.cn), create a self-built app.
2. Enable the bot capability, and grant the message and chat permissions the
   adapter needs (`im:message`, `im:message:send_as_bot`, `im:chat:readonly`).
3. Subscribe to events over the long connection: `im.message.receive_v1` and
   `card.action.trigger`. No public callback URL is needed.
4. Publish the app and add it to the target chat.
5. In Vibex, open **Settings → IM channels**, add a Lark channel with the app id,
   the app secret, the deployment, and optionally the chat id, then test and
   enable it.

The app secret is written to the OS keychain on a desktop, or to the runtime's
host secret file on a headless deployment. Deleting the channel erases it.

## Pair your chat

A channel refuses everyone until an identity is paired.

1. In **Settings → IM channels**, open the channel and press **Generate pairing
   code**. The code looks like `VBX-ABC12-DEF34` and expires in 10 minutes.
2. Send `/pair VBX-ABC12-DEF34` to the bot in a direct message.
3. The bot confirms the pairing.

A code is bound to the kind of chat it was minted for. A code generated in the
app is a direct-message code, and sending it in a group is refused; a group code
names one chat and is refused anywhere else. A second person cannot reuse a code
that was already claimed.

### Pairing a group

A group needs its own code, because a direct code pasted into a group would
otherwise pair everyone who saw it. Mint one from the command line, naming the
chat:

```bash
vibex-server im pair --channel "Team bot" --chat -1001234567890
```

Then send `/pair <code>` in that group. The claimed identity is the person who
sent it; other members still have to be paired individually.

An unpaired group is otherwise ignored: the bot does not answer non-command
messages there, so it stays quiet in a busy room.

### Who may do what

Pairing grants full control: anyone paired can prompt the Agent, approve or deny
what it asks, and unbind the chat with `/unbind`. A channel that needs read-only
watchers should use a separate bot rather than a smaller role, because every
paired identity on a channel is trusted the same way.

## Commands

| Command | What it does |
| --- | --- |
| `/pair <code>` | pair this chat |
| `/help` | the short list; `/help commands` lists everything |
| `/whoami` | your identity |
| `/status` | bound session, runtime, state, pending approvals |
| `/sessions` | recent sessions, one button each |
| `/use <n or id>` | bind this chat to a session |
| `/agent` | switch Agent |
| `/provider [id]` | pick the credential source for this Agent |
| `/model [id]` | pick the model |
| `/effort [value]` | pick the reasoning effort |
| `/mode [id]` | pick the Agent mode |
| `/new <text>` | start a session and send the first prompt |
| `/stop` | interrupt the running turn |
| `/approve [code]` | approve the pending request |
| `/always [code]` | allow for the rest of the session |
| `/deny [code]` | deny |
| `/say <text>` | send text that starts with `/` |
| `/unbind` | drop this chat's binding (admin) |

Telegram publishes this table as the bot's command menu, so the `/` picker in a
client offers the same names. Where a command takes a choice, the reply carries
buttons and the text form still works; every button also prints a short code for
clients that cannot show an inline keyboard.

In a group, the bot answers commands and — on Telegram — messages that begin
with its `@username`. The mention requirement is a per-channel setting; with it
off, any text in the group reaches the Agent.

### Changing the session

Each command owns one part of the runtime selection of the session this chat is
bound to: `/agent` switches Agent (starting from a source and model that Agent
advertises), `/provider` changes the credential source without changing the
Agent, and `/model`, `/effort` and `/mode` narrow it from there. Switching Agent
or provider clears the reasoning effort, because the new one advertises its own.

Every picker shows what the current selection advertises, the choice applies to
the next turn, and the card carries the selection revisions it was read at, so a
card left open while someone else changed the runtime is refused instead of
overwriting the newer choice.

### `/new` and defaults

`/new` needs a workspace and an Agent. It uses the channel's default session
when one is set in settings; otherwise it copies the workspace and runtime
selection of the session this chat is currently bound to. With neither, it
answers with instructions instead of guessing.

### Long answers

A final answer longer than one chat message is split on paragraph, line, then
word boundaries and sent as `(1/3)`, `(2/3)`, `(3/3)`. Parts go out in order and
share the channel's send queue, so a provider rate limit delays the tail rather
than reordering it.

## Approvals

When the Agent asks for permission, the channel posts a card with one button per
option the provider offered: **Approve**, **Always allow**, and **Deny**,
depending on what it accepts.

- On Telegram the buttons are an inline keyboard.
- On Lark they are an interactive card.
- Either way the card also prints a short code, and `/approve <code>` works if
  buttons are unavailable.

Every button carries a one-shot token. A second tap, an expired card, or a
button from another chat is refused, and the first tap wins even if the desktop
is pressing approve at the same moment.

## Images and documents

Send a photo or a document to the bot in a paired chat and it becomes an
attachment on the prompt:

- Telegram: photos at their largest size, documents whose type is an image, text,
  JSON, or PDF.
- Lark: images and files from the message.
- The cap is 10 MB; a larger file is rejected with a reason instead of being
  dropped silently.
- The Agent receives the bytes, not a provider URL, so nothing fetches a remote
  file on your behalf.

## Headless deployments

A desktop closes and takes its channels with it. For 7×24 service, run
`vibex-server` on a host that stays up; it starts the IM service with the rest of
the runtime. Manage channels over SSH:

```bash
vibex-server im list
vibex-server im list --json

# Mint a code for someone else to claim.
vibex-server im pair --channel "Team bot" --ttl-minutes 10

vibex-server im bindings --channel "Team bot"
vibex-server im unbind --binding <binding-id>

vibex-server im test --channel "Team bot"
vibex-server im enable --channel "Team bot"
vibex-server im disable --channel "Team bot"

vibex-server im revoke --channel "Team bot" --principal <id-or-external-id>
```

`--channel` accepts the channel id or its display name. `revoke` accepts either
the Vibex principal id or the external id the chat shows.

`im enable` writes the enabled flag; the connection itself belongs to the
process that owns the home directory, so a running daemon picks the change up on
its next start.

## Where state lives

| What | Where |
| --- | --- |
| Channels, pairings, bindings, cards | the runtime SQLite database |
| Bot tokens and app secrets | OS keychain, or `<home>/im-secrets.json` headless |
| Audit of pairing and approval | `im_audit_logs`, kept 180 days |
| Delivered and received messages | `im_messages`, kept 14 days |
| Approval outcome | the session timeline, as a `PermissionResolution` |

Every approval writes both places: the timeline records who resolved it and
that it came through IM, and the audit log records the channel, binding, and
principal.

Old rows are pruned at start-up and every six hours. The transcript is the
session timeline, so a message that was delivered is still readable there after
the log entry is gone.

### Diagnostics

A diagnostic bundle includes an `imChannels` section: one row per channel with
its kind, whether it is enabled and configured, and the connection state. It
carries no chat id, session id, or message text, because a bundle is meant to be
shared with support.

## Troubleshooting

| Symptom | Cause |
| --- | --- |
| `This bot is not paired with you yet` | the code was never sent, or it expired |
| `This action is already handled` | someone else resolved it first, or you tapped twice |
| `This action belongs to another chat` | the card was forwarded |
| `the channel secret is not configured` | the token was cleared; set it again in settings |
| Bot silent in a group | privacy mode hides non-command messages, or the mention setting is on and you did not mention it |
| Bot offline after closing the desktop | the desktop hosted the connection; run `vibex-server` |
| Lark channel degrades with an auth error | the app secret was rotated, or the app is unpublished |

### Rate limits

A chat that sends faster than a person is throttled rather than served:

| Scope | Limit |
| --- | --- |
| One identity | 20 messages per minute |
| One chat | 40 messages per minute |
| Pairing attempts | 5 per chat per 10 minutes |

A refused message gets one reply with the retry delay. Approval and answer
buttons are never throttled: a press is a response to a card that was already
delivered.

Timestamped delivery problems are visible in the channel's status: last inbound,
last outbound, pending sends, and the last error with its retry time.

### Falling behind

Live events reach a channel over a process-local stream with a bounded buffer.
When the runtime produces faster than a chat can be written to, the buffer
overwrites its oldest entries and the channel reports how many it dropped; the
count is logged, not shown in the chat.

A dropped batch is drained beside the stream, one drain at a time, so the
channel keeps reading while it catches up. A chat is told once per backlog — one
line suggesting `/status` — rather than once per batch, and only when it was not
already told about the same run of drops. An approval card is re-sent only if its
message never left the process: a card already on screen keeps the buttons that
resolve it. A reply that was lost is not replayed, because the stream is not a
durable log; open Vibex for the transcript.

## Limits

- One runtime per home directory owns the connections. A second process cannot
  serve the same channels.
- A question with exactly one boolean field, or one field restricted to a fixed
  option list, is answered with a button. Any other form — several fields, free
  text, numbers, multi-select — sends a notice and the answer happens in Vibex.
- Reasoning is never forwarded, and tool names only label the one live progress
  line. Final answers, errors, approvals, notices, and the Agent's reaction are
  what a chat receives; a channel that removes `tool_progress` from its event
  filter gets a progress line without them.
- Lark card buttons require the `card.action.trigger` event subscription; without
  it approvals fall back to `/approve <code>`.
