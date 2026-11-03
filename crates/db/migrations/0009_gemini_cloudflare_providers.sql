-- Add Gemini Developer API and Cloudflare Workers AI to the sealed provider
-- configuration registry. Existing provider rows remain valid.
ALTER TABLE provider_configs DROP CONSTRAINT provider_configs_provider_known;
ALTER TABLE provider_configs
    ADD CONSTRAINT provider_configs_provider_known CHECK (provider IN (
        'bedrock', 'openai', 'anthropic', 'openrouter',
        'deepseek', 'grok', 'huggingface', 'gemini', 'cloudflare', 'openai-compat'
    ));
