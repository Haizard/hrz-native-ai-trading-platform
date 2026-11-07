-- Add Groq as a separately named OpenAI-compatible provider. `grok` remains xAI.
ALTER TABLE provider_configs DROP CONSTRAINT provider_configs_provider_known;
ALTER TABLE provider_configs
    ADD CONSTRAINT provider_configs_provider_known CHECK (provider IN (
        'bedrock', 'openai', 'anthropic', 'openrouter',
        'deepseek', 'grok', 'groq', 'huggingface', 'gemini', 'cloudflare', 'openai-compat'
    ));
