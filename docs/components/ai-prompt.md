# AI Prompt component

> The `ai-prompt` component turns a route into a prompt template an AI client can offer its user: the route renders the prompt from the arguments, and its body is the prompt text or a list of messages.

**Scheme:** `ai-prompt:` · **Crate:** `cameleer-ai` · **Cargo feature:** [`ai`](../guides/build-features.md) ·
**Consumer:** yes · **Producer:** yes · **Apache Camel:** no equivalent (Cameleer extension)

## When to use it

- Ship prompts that use live data: a review prompt that includes the style guide, a support
  prompt that includes the customer's open tickets.
- Keep prompts versioned with the routes that serve the data they use.

## URI format

```text
ai-prompt:name[?options]
```

## Example

<!-- example: ai-prompt-code-review -->
```yaml
- route:
    from:
      uri: ai-prompt:code-review
      parameters:
        description: Review code against our style guide
        argument.code.description: The code to review
        argument.code.required: true
        argument.language: The programming language
      steps:
        - setBody:
            simple: "Review this ${header.language} code against our style guide:\n${header.code}"
```

A body that is a list of messages sets the roles:

```json
[{"role": "user", "content": "Summarise this ticket"}, {"role": "assistant", "content": "Which ticket?"}]
```

From Rust, `AiRegistry::of(&ctx)?.get_prompt("code-review", &args)?` returns the messages.
From a route, `to: ai-prompt:code-review` with a JSON object of arguments as the body returns
the messages as a list of `{role, content}` maps.

## Options

| option | default | description |
|---|---|---|
| `title` | | A human-readable name |
| `description` | | What the prompt is for |
| `argument.NAME` | | Declares an argument with this description |
| `argument.NAME.description` | | The argument's description |
| `argument.NAME.required` | `false` | Whether the argument is required |
| `argument.NAME.values` | | Comma-separated completion values for the argument (MCP `completion/complete`) |
| `tags` | | Comma-separated tags |

## Message headers

| header | direction | description |
|---|---|---|
| each declared argument, by name | in | The argument values (undeclared ones are dropped) |
| `CamelAiPromptName` | in | The prompt's name |

## Behavior

- A text body becomes one `user` message. A list of `{role, content}` maps (roles `user` and
  `assistant`) becomes those messages. A `content` that is an MCP content block (an image, an
  embedded resource) is passed on as it is.
- A missing required argument fails before the route runs.
- An unknown prompt fails with `PromptNotFoundException`.

## Related

- [ai-tool](ai-tool.md) and [ai-resource](ai-resource.md).
- [Cameleer for AI agents](../guides/agents.md).

## Found a problem?

If something on this page doesn't match what Cameleer does, please
[open an issue](https://github.com/pmsudhi/cameleer/issues/new), or fork the repository, fix
it, add a test, run `cargo test --workspace` and open a pull request.
