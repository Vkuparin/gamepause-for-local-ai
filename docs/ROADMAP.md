# Roadmap ideas

Ideas recorded for a later release. Nothing here is implemented or promised.

## After 2.0

- **Image-generation tools as providers.** ComfyUI and the Stable Diffusion web UIs hold as much VRAM as a language model. They would fit the unload-only path: free their models for gaming and let the tool reload lazily afterwards, with no restore obligation. Their HTTP APIs have not been checked, and the owner has no installation to test against, so this would start as source/fixture work like the first Ollama adapter.
