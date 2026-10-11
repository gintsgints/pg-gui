# AI completion

Optional: completes the SQL at the cursor using the Claude API. `cmd-i` or
`ctrl-space`, or Edit ▸ AI Complete.

The model receives the text before and after the cursor and the completion is
inserted at the cursor.

## Enabling it

Set an API key, either as `ai_api_key` in `config.json` or in the environment:

```sh
export ANTHROPIC_API_KEY=sk-ant-...
cargo run
```

Without a key the menu item stays disabled; nothing is sent anywhere and no
other feature depends on it.

## Configuration

| Setting | Default | Notes |
| --- | --- | --- |
| `ai_api_key` (`config.json`) or `ANTHROPIC_API_KEY` | — | Required to enable the feature |
| `ai_model` (`config.json`) or `PG_GUI_AI_MODEL` | `claude-opus-4-8` | The config wins over the environment; e.g. `claude-haiku-4-5` for lower latency |
| `ai_prompt` (`config.json`) | — | Appended to the system prompt |

`ai_prompt` is where schema hints or style preferences go — for example
`"Prefer explicit JOINs and snake_case aliases."`.

## What leaves the machine

The buffer text around the cursor goes to the Claude API when you invoke the
command, and only then. The completions in the editor's own menu come from the
[embedded language server](language-server.md) and the
[snippets](snippets.md), which are entirely local.
