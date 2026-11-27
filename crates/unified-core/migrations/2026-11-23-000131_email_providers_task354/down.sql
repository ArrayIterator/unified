-- Reverse of 000131: remove the 43 email providers
-- and the five columns it added. Only the 43 built-in ids are
-- deleted; rows an admin registered themselves are untouched. The
-- updated notes on the 13 existing providers are NOT reverted (the
-- old wording is not preserved anywhere in SQL; the seed JSON is
-- the source of truth). The runtime registry falls back to the
-- embedded seed document, so the providers keep working in memory
-- until the seed is reverted too.
DELETE FROM provider_registry_definitions
    WHERE family = 'email'
      AND provider_id IN ('zoho', 'zoho-eu', 'proton', 'yandex', 'fastmail', 'icloud', 'gmx', 'mail-com', 'aol', 'qq-mail', 'mail-163', 'mail-126', 'sina', 'mail-ru', 'posteo', 'migadu', 'mailfence', 'startmail', 'runbox', 'kolabnow', 'infomaniak', 'ovh', 'gandi', 'namecheap', 'dreamhost', 'mxroute', 'sender-net', 'sendpulse', 'mailjet', 'mailtrap', 'elastic-email', 'smtp2go', 'mailersend', 'zeptomail', 'sendlayer', 'socketlabs', 'smtp-com', 'campaign-monitor', 'plunk', 'courier', 'loops', 'onesignal');
ALTER TABLE provider_registry_definitions
    DROP COLUMN IF EXISTS smtp_host,
    DROP COLUMN IF EXISTS smtp_port,
    DROP COLUMN IF EXISTS smtp_tls,
    DROP COLUMN IF EXISTS http_api,
    DROP COLUMN IF EXISTS affiliate_url;
