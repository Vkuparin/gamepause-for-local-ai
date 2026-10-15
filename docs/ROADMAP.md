# Roadmap ideas

The completed v2.6.0 work is recorded below, followed by ideas for later releases.

## v2.6.0: Hermes local models

Implemented first-class support for Hermes's managed local runtime, with a dashboard that fits LM Studio, Ollama and Hermes plus Other AI apps. Native runtime control changes Hermes's persistent enabled setting, with that obligation captured in recovery before mutation.

See the [task record](v2.6.0-tasks.md) and [review](hermes-local-runtime-review.md). A focused live native pause/start and single-model replay passed on CUDA llama.cpp b11370. Broader live scenarios remain untested.

## After 2.0

- **Image-generation tools as providers.** ComfyUI and the Stable Diffusion web UIs hold as much VRAM as a language model. They would fit the unload-only path: free their models for gaming and let the tool reload lazily afterwards, with no restore obligation. Their HTTP APIs have not been checked, and the owner has no installation to test against, so this would start as source/fixture work like the first Ollama adapter.
