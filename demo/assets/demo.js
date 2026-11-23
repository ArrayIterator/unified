/* ==========================================================================
   Unified demo — shared script (phase 1)
   --------------------------------------------------------------------------
   One source of truth for the shared shell: every page ships only its
   <main> content plus <body data-page="..." data-layout="app|auth"> and
   this script renders the topbar, sidebar, footer and toast around it.
   Layouts: "app" (topbar + sidebar + footer, signed-in shell),
   "public" (topbar + footer, no sidebar — public pages such as the home
   and tenant front pages), "auth" (centred, no sidebar).

   HOW TO ADD A PAGE (phase 2):
   1. Copy any existing page. Keep the <head> block verbatim (stylesheet,
      theme bootstrap snippet, sprite.js + this script at the end of body).
   2. Set <body data-page="your-page" data-layout="app"> (or "public" for a
      public page with no sidebar, "auth" for centred auth-style pages)
   3. If the page needs top-level navigation, add one entry to NAV below
      and, when it is a section of its own, to SIDEBAR_NAV. Do not copy
      the header/sidebar markup into the page.
   4. Use relative links only, kebab-case module ids for icons
      (<svg class="module-icon"><use href="#module-icon-billing"/></svg>),
      and English demo data. Forms get data-demo-form (no real submit).

   Conventions: theme choice persists in localStorage key THEME_KEY;
   icons come from assets/sprite.js (injected below, works from file://);
   every interactive control is demo state only and shows a toast.
   ========================================================================== */

(function () {
  "use strict";

  var THEME_KEY = "unified-demo-theme";

  /* ---------- Theme switcher (demo only) ---------- */

  function applyTheme(name) {
    var dark = name === "editorial";
    document.documentElement.classList.toggle("theme-dark", dark);
    try {
      localStorage.setItem(THEME_KEY, dark ? "editorial" : "default");
    } catch (e) {
      /* Storage unavailable (private mode) — theme still applies. */
    }
    syncThemeControls(dark ? "editorial" : "default");
  }

  function syncThemeControls(name) {
    var buttons = document.querySelectorAll("[data-theme-switch]");
    for (var i = 0; i < buttons.length; i++) {
      buttons[i].setAttribute(
        "aria-pressed",
        buttons[i].getAttribute("data-theme-switch") === name ? "true" : "false"
      );
    }
    var picks = document.querySelectorAll("[data-theme-pick]");
    for (var j = 0; j < picks.length; j++) {
      picks[j].setAttribute(
        "aria-pressed",
        picks[j].getAttribute("data-theme-pick") === name ? "true" : "false"
      );
    }
  }

  function currentTheme() {
    try {
      return localStorage.getItem(THEME_KEY) === "editorial" ? "editorial" : "default";
    } catch (e) {
      return "default";
    }
  }

  /* ---------- Shared navigation ---------- */

  /* Topbar entries. `page` matches <body data-page="..."> for the active state. */
  var NAV = [
    { page: "home", label: "Home", href: "index.html" },
    { page: "dashboard", label: "Dashboard", href: "dashboard.html" },
    { page: "tenant", label: "Tenant demo", href: "tenant.html" },
    { page: "modules", label: "Modules", href: "modules.html" },
    { page: "store", label: "Store", href: "store.html" },
    { page: "admin", label: "Admin", href: "admin.html" },
    { page: "settings", label: "Settings", href: "settings.html" }
  ];

  /* Sidebar module shortcuts (real module ids, kebab-case, matching the sprite). */
  var SIDEBAR_MODULES = [
    { id: "billing", label: "Billing", href: "module.html" },
    { id: "crm", label: "CRM", href: "modules.html" },
    { id: "content", label: "Content", href: "modules.html" },
    { id: "documents", label: "Documents", href: "modules.html" },
    { id: "licensing", label: "Licensing", href: "modules.html" },
    { id: "users", label: "Users", href: "modules.html" },
    { id: "security", label: "Security", href: "modules.html" },
    { id: "store", label: "Store", href: "store.html" }
  ];

  function icon(id, large) {
    return (
      '<svg class="module-icon' + (large ? " module-icon-lg" : "") +
      '" aria-hidden="true"><use href="#module-icon-' + id + '"></use></svg>'
    );
  }


  /* ---------- Site settings data (task 351) ---------- */

  /* Full IANA timezone list (420 zones, UTC first) — the same data
     as the real settings API's site definitions. */
  var IANA_TIMEZONES = ["UTC", "Africa/Abidjan", "Africa/Accra", "Africa/Addis_Ababa", "Africa/Algiers", "Africa/Asmara", "Africa/Bamako", "Africa/Bangui", "Africa/Banjul", "Africa/Bissau", "Africa/Blantyre", "Africa/Brazzaville", "Africa/Bujumbura", "Africa/Cairo", "Africa/Casablanca", "Africa/Ceuta", "Africa/Conakry", "Africa/Dakar", "Africa/Dar_es_Salaam", "Africa/Djibouti", "Africa/Douala", "Africa/El_Aaiun", "Africa/Freetown", "Africa/Gaborone", "Africa/Harare", "Africa/Johannesburg", "Africa/Juba", "Africa/Kampala", "Africa/Khartoum", "Africa/Kigali", "Africa/Kinshasa", "Africa/Lagos", "Africa/Libreville", "Africa/Lome", "Africa/Luanda", "Africa/Lubumbashi", "Africa/Lusaka", "Africa/Malabo", "Africa/Maputo", "Africa/Maseru", "Africa/Mbabane", "Africa/Mogadishu", "Africa/Monrovia", "Africa/Nairobi", "Africa/Ndjamena", "Africa/Niamey", "Africa/Nouakchott", "Africa/Ouagadougou", "Africa/Porto-Novo", "Africa/Sao_Tome", "Africa/Tripoli", "Africa/Tunis", "Africa/Windhoek", "America/Adak", "America/Anchorage", "America/Anguilla", "America/Antigua", "America/Araguaina", "America/Argentina/Buenos_Aires", "America/Argentina/Catamarca", "America/Argentina/Cordoba", "America/Argentina/Jujuy", "America/Argentina/La_Rioja", "America/Argentina/Mendoza", "America/Argentina/Rio_Gallegos", "America/Argentina/Salta", "America/Argentina/San_Juan", "America/Argentina/San_Luis", "America/Argentina/Tucuman", "America/Argentina/Ushuaia", "America/Aruba", "America/Asuncion", "America/Atikokan", "America/Bahia", "America/Bahia_Banderas", "America/Barbados", "America/Belem", "America/Belize", "America/Blanc-Sablon", "America/Boa_Vista", "America/Bogota", "America/Boise", "America/Cambridge_Bay", "America/Campo_Grande", "America/Cancun", "America/Caracas", "America/Cayenne", "America/Cayman", "America/Chicago", "America/Chihuahua", "America/Ciudad_Juarez", "America/Costa_Rica", "America/Coyhaique", "America/Creston", "America/Cuiaba", "America/Curacao", "America/Danmarkshavn", "America/Dawson", "America/Dawson_Creek", "America/Denver", "America/Detroit", "America/Dominica", "America/Edmonton", "America/Eirunepe", "America/El_Salvador", "America/Fort_Nelson", "America/Fortaleza", "America/Glace_Bay", "America/Goose_Bay", "America/Grand_Turk", "America/Grenada", "America/Guadeloupe", "America/Guatemala", "America/Guayaquil", "America/Guyana", "America/Halifax", "America/Havana", "America/Hermosillo", "America/Indiana/Indianapolis", "America/Indiana/Knox", "America/Indiana/Marengo", "America/Indiana/Petersburg", "America/Indiana/Tell_City", "America/Indiana/Vevay", "America/Indiana/Vincennes", "America/Indiana/Winamac", "America/Inuvik", "America/Iqaluit", "America/Jamaica", "America/Juneau", "America/Kentucky/Louisville", "America/Kentucky/Monticello", "America/La_Paz", "America/Lima", "America/Los_Angeles", "America/Maceio", "America/Managua", "America/Manaus", "America/Martinique", "America/Matamoros", "America/Mazatlan", "America/Menominee", "America/Merida", "America/Metlakatla", "America/Mexico_City", "America/Miquelon", "America/Moncton", "America/Monterrey", "America/Montevideo", "America/Montserrat", "America/Nassau", "America/New_York", "America/Nome", "America/Noronha", "America/North_Dakota/Beulah", "America/North_Dakota/Center", "America/North_Dakota/New_Salem", "America/Nuuk", "America/Ojinaga", "America/Panama", "America/Paramaribo", "America/Phoenix", "America/Port-au-Prince", "America/Port_of_Spain", "America/Porto_Velho", "America/Puerto_Rico", "America/Punta_Arenas", "America/Rankin_Inlet", "America/Recife", "America/Regina", "America/Resolute", "America/Rio_Branco", "America/Santarem", "America/Santiago", "America/Santo_Domingo", "America/Sao_Paulo", "America/Scoresbysund", "America/Sitka", "America/St_Johns", "America/St_Kitts", "America/St_Lucia", "America/St_Thomas", "America/St_Vincent", "America/Swift_Current", "America/Tegucigalpa", "America/Thule", "America/Tijuana", "America/Toronto", "America/Tortola", "America/Vancouver", "America/Whitehorse", "America/Winnipeg", "America/Yakutat", "Antarctica/Casey", "Antarctica/Davis", "Antarctica/DumontDUrville", "Antarctica/Macquarie", "Antarctica/Mawson", "Antarctica/McMurdo", "Antarctica/Palmer", "Antarctica/Rothera", "Antarctica/Syowa", "Antarctica/Troll", "Antarctica/Vostok", "Asia/Aden", "Asia/Almaty", "Asia/Amman", "Asia/Anadyr", "Asia/Aqtau", "Asia/Aqtobe", "Asia/Ashgabat", "Asia/Atyrau", "Asia/Baghdad", "Asia/Bahrain", "Asia/Baku", "Asia/Bangkok", "Asia/Barnaul", "Asia/Beirut", "Asia/Bishkek", "Asia/Brunei", "Asia/Chita", "Asia/Colombo", "Asia/Damascus", "Asia/Dhaka", "Asia/Dili", "Asia/Dubai", "Asia/Dushanbe", "Asia/Famagusta", "Asia/Gaza", "Asia/Hebron", "Asia/Ho_Chi_Minh", "Asia/Hong_Kong", "Asia/Hovd", "Asia/Irkutsk", "Asia/Jakarta", "Asia/Jayapura", "Asia/Jerusalem", "Asia/Kabul", "Asia/Kamchatka", "Asia/Karachi", "Asia/Kathmandu", "Asia/Khandyga", "Asia/Kolkata", "Asia/Krasnoyarsk", "Asia/Kuala_Lumpur", "Asia/Kuching", "Asia/Kuwait", "Asia/Macau", "Asia/Magadan", "Asia/Makassar", "Asia/Manila", "Asia/Muscat", "Asia/Nicosia", "Asia/Novokuznetsk", "Asia/Novosibirsk", "Asia/Omsk", "Asia/Oral", "Asia/Phnom_Penh", "Asia/Pontianak", "Asia/Pyongyang", "Asia/Qatar", "Asia/Qostanay", "Asia/Qyzylorda", "Asia/Riyadh", "Asia/Sakhalin", "Asia/Samarkand", "Asia/Seoul", "Asia/Shanghai", "Asia/Singapore", "Asia/Srednekolymsk", "Asia/Taipei", "Asia/Tashkent", "Asia/Tbilisi", "Asia/Tehran", "Asia/Thimphu", "Asia/Tokyo", "Asia/Tomsk", "Asia/Ulaanbaatar", "Asia/Urumqi", "Asia/Ust-Nera", "Asia/Vientiane", "Asia/Vladivostok", "Asia/Yakutsk", "Asia/Yangon", "Asia/Yekaterinburg", "Asia/Yerevan", "Atlantic/Azores", "Atlantic/Bermuda", "Atlantic/Canary", "Atlantic/Cape_Verde", "Atlantic/Faroe", "Atlantic/Madeira", "Atlantic/Reykjavik", "Atlantic/South_Georgia", "Atlantic/St_Helena", "Atlantic/Stanley", "Australia/Adelaide", "Australia/Brisbane", "Australia/Broken_Hill", "Australia/Darwin", "Australia/Eucla", "Australia/Hobart", "Australia/Lindeman", "Australia/Lord_Howe", "Australia/Melbourne", "Australia/Perth", "Australia/Sydney", "CET", "CST6CDT", "EET", "EST", "EST5EDT", "Europe/Amsterdam", "Europe/Andorra", "Europe/Astrakhan", "Europe/Athens", "Europe/Belgrade", "Europe/Berlin", "Europe/Brussels", "Europe/Bucharest", "Europe/Budapest", "Europe/Chisinau", "Europe/Copenhagen", "Europe/Dublin", "Europe/Gibraltar", "Europe/Guernsey", "Europe/Helsinki", "Europe/Isle_of_Man", "Europe/Istanbul", "Europe/Jersey", "Europe/Kaliningrad", "Europe/Kirov", "Europe/Kyiv", "Europe/Lisbon", "Europe/Ljubljana", "Europe/London", "Europe/Luxembourg", "Europe/Madrid", "Europe/Malta", "Europe/Minsk", "Europe/Monaco", "Europe/Moscow", "Europe/Oslo", "Europe/Paris", "Europe/Prague", "Europe/Riga", "Europe/Rome", "Europe/Samara", "Europe/Sarajevo", "Europe/Saratov", "Europe/Simferopol", "Europe/Skopje", "Europe/Sofia", "Europe/Stockholm", "Europe/Tallinn", "Europe/Tirane", "Europe/Ulyanovsk", "Europe/Vaduz", "Europe/Vienna", "Europe/Vilnius", "Europe/Volgograd", "Europe/Warsaw", "Europe/Zagreb", "Europe/Zurich", "Factory", "HST", "Indian/Antananarivo", "Indian/Chagos", "Indian/Christmas", "Indian/Cocos", "Indian/Comoro", "Indian/Kerguelen", "Indian/Mahe", "Indian/Maldives", "Indian/Mauritius", "Indian/Mayotte", "Indian/Reunion", "MET", "MST", "MST7MDT", "PST8PDT", "Pacific/Apia", "Pacific/Auckland", "Pacific/Bougainville", "Pacific/Chatham", "Pacific/Chuuk", "Pacific/Easter", "Pacific/Efate", "Pacific/Fakaofo", "Pacific/Fiji", "Pacific/Funafuti", "Pacific/Galapagos", "Pacific/Gambier", "Pacific/Guadalcanal", "Pacific/Guam", "Pacific/Honolulu", "Pacific/Kanton", "Pacific/Kiritimati", "Pacific/Kosrae", "Pacific/Kwajalein", "Pacific/Majuro", "Pacific/Marquesas", "Pacific/Midway", "Pacific/Nauru", "Pacific/Niue", "Pacific/Norfolk", "Pacific/Noumea", "Pacific/Pago_Pago", "Pacific/Palau", "Pacific/Pitcairn", "Pacific/Pohnpei", "Pacific/Port_Moresby", "Pacific/Rarotonga", "Pacific/Saipan", "Pacific/Tahiti", "Pacific/Tarawa", "Pacific/Tongatapu", "Pacific/Wake", "Pacific/Wallis", "WET"];

  /* ---------- Mail providers (task 350) ---------- */

  /* The mail provider catalogue: mirrors the real catalog endpoint
     (GET /api/v1/email-providers/catalog) — kind, name, logo stem,
     one-line note, and the provider's own settings fields. The demo
     renders the picker grid and the form from this data, so adding a
     provider here is the only change a new provider needs. */
  var DEMO_MAIL_PROVIDERS = [{"id": "sendmail", "name": "Local Sendmail", "logo": "email-sendmail", "note": "Free (local binary, no provider). Best for: servers with a local MTA where mail never leaves the box.", "fields": [{"key": "sender_email", "label": "Default sender email", "type": "email", "placeholder": "noreply@example.com", "help": "From address used when a send does not supply its own; defaults to unified@localhost."}, {"key": "sender_name", "label": "Default sender name", "type": "text", "placeholder": "Unified"}, {"key": "test_mode", "label": "Test mode (sandbox)", "type": "checkbox", "help": "When on, mail is recorded as a simulated delivery and never sent."}]}, {"id": "smtp", "name": "SMTP (Custom server)", "logo": "email-smtp", "note": "No provider pricing \u2014 your own server. Best for: any SMTP server when you already run one.", "fields": [{"key": "host", "label": "SMTP host", "type": "text", "required": true, "placeholder": "smtp.example.com"}, {"key": "port", "label": "SMTP port", "type": "number", "required": true, "placeholder": "587", "help": "587 for STARTTLS, 465 for implicit TLS."}, {"key": "tls_mode", "label": "TLS mode", "type": "select", "required": true, "options": [["starttls", "STARTTLS (port 587)"], ["implicit", "Implicit TLS (port 465)"], ["none", "None (plaintext, local servers only)"]]}, {"key": "username", "label": "SMTP username", "type": "text", "placeholder": "user@example.com", "help": "Usually the sender address. Leave empty for servers without authentication."}, {"key": "password", "label": "SMTP password", "type": "password", "help": "Stored encrypted; never shown again."}, {"key": "sender_email", "label": "Default sender email", "type": "email", "required": true, "placeholder": "news@example.com"}, {"key": "sender_name", "label": "Default sender name", "type": "text", "placeholder": "Unified"}, {"key": "test_mode", "label": "Test mode (sandbox)", "type": "checkbox", "help": "When on, mail is recorded as a simulated delivery and never sent."}]}, {"id": "gmail", "name": "Gmail", "logo": "email-gmail", "note": "Free; Gmail daily sending limits apply. Best for: sending from a personal or Google Workspace account over OAuth2.", "oauth": "Google", "fields": [{"key": "client_id", "label": "OAuth client ID", "type": "text", "required": true}, {"key": "client_secret", "label": "OAuth client secret", "type": "password", "required": true, "help": "Stored encrypted; never shown again."}, {"key": "sender_email", "label": "Sender email", "type": "email", "required": true, "placeholder": "you@gmail.com", "help": "The linked account address \u2014 the XOAUTH2 identity."}, {"key": "sender_name", "label": "Default sender name", "type": "text", "placeholder": "Unified"}, {"key": "test_mode", "label": "Test mode (sandbox)", "type": "checkbox", "help": "When on, mail is recorded as a simulated delivery and never sent."}]}, {"id": "yahoo", "name": "Yahoo Mail", "logo": "email-yahoo", "note": "Free. Best for: sending from a Yahoo Mail account over OAuth2.", "oauth": "Yahoo", "fields": [{"key": "client_id", "label": "OAuth client ID", "type": "text", "required": true}, {"key": "client_secret", "label": "OAuth client secret", "type": "password", "required": true, "help": "Stored encrypted; never shown again."}, {"key": "sender_email", "label": "Sender email", "type": "email", "required": true, "placeholder": "you@yahoo.com", "help": "The linked account address \u2014 the XOAUTH2 identity."}, {"key": "sender_name", "label": "Default sender name", "type": "text", "placeholder": "Unified"}, {"key": "test_mode", "label": "Test mode (sandbox)", "type": "checkbox", "help": "When on, mail is recorded as a simulated delivery and never sent."}]}, {"id": "outlook", "name": "Outlook / Microsoft 365", "logo": "email-outlook", "note": "Free. Best for: sending from an Outlook, Microsoft 365 or Hotmail account over OAuth2.", "oauth": "Microsoft", "fields": [{"key": "client_id", "label": "OAuth client ID", "type": "text", "required": true}, {"key": "client_secret", "label": "OAuth client secret", "type": "password", "required": true, "help": "Stored encrypted; never shown again."}, {"key": "sender_email", "label": "Sender email", "type": "email", "required": true, "placeholder": "you@outlook.com", "help": "The linked account address \u2014 the XOAUTH2 identity."}, {"key": "sender_name", "label": "Default sender name", "type": "text", "placeholder": "Unified"}, {"key": "test_mode", "label": "Test mode (sandbox)", "type": "checkbox", "help": "When on, mail is recorded as a simulated delivery and never sent."}]}, {"id": "sendgrid", "name": "SendGrid", "logo": "email-sendgrid", "note": "60-day trial only, then paid. Best for: high-volume senders who need deliverability tooling and a sandbox.", "fields": [{"key": "api_key", "label": "API key", "type": "password", "required": true}, {"key": "sender_email", "label": "Default sender email", "type": "email", "placeholder": "news@example.com", "help": "From address used when a send does not supply its own."}, {"key": "sender_name", "label": "Default sender name", "type": "text", "placeholder": "Unified"}, {"key": "webhook_secret", "label": "Webhook secret / signing key", "type": "password", "help": "Secret used to verify incoming webhooks from this provider."}, {"key": "test_mode", "label": "Test mode (sandbox)", "type": "checkbox", "help": "When on, mail is recorded as a simulated delivery and never sent."}]}, {"id": "mailgun", "name": "Mailgun", "logo": "email-mailgun", "note": "Free 100 emails/day. Best for: developers who want a programmable transactional API with sandbox domains.", "fields": [{"key": "api_key", "label": "API key", "type": "password", "required": true}, {"key": "domain", "label": "Mailgun domain", "type": "text", "required": true, "placeholder": "mg.example.com"}, {"key": "region", "label": "Region", "type": "select", "options": [["us", "US (api.mailgun.net)"], ["eu", "EU (api.eu.mailgun.net)"]]}, {"key": "sender_email", "label": "Default sender email", "type": "email", "placeholder": "news@example.com", "help": "From address used when a send does not supply its own."}, {"key": "sender_name", "label": "Default sender name", "type": "text", "placeholder": "Unified"}, {"key": "webhook_secret", "label": "Webhook secret / signing key", "type": "password", "help": "Secret used to verify incoming webhooks from this provider."}, {"key": "test_mode", "label": "Test mode (sandbox)", "type": "checkbox", "help": "When on, mail is recorded as a simulated delivery and never sent."}]}, {"id": "postmark", "name": "Postmark", "logo": "email-postmark", "note": "Free 100 emails/month, never expires. Best for: transactional-only sending where deliverability matters most.", "fields": [{"key": "api_key", "label": "API key", "type": "password", "required": true}, {"key": "sender_email", "label": "Default sender email", "type": "email", "placeholder": "news@example.com", "help": "From address used when a send does not supply its own."}, {"key": "sender_name", "label": "Default sender name", "type": "text", "placeholder": "Unified"}, {"key": "webhook_secret", "label": "Webhook secret / signing key", "type": "password", "help": "Secret used to verify incoming webhooks from this provider."}, {"key": "test_mode", "label": "Test mode (sandbox)", "type": "checkbox", "help": "When on, mail is recorded as a simulated delivery and never sent."}]}, {"id": "ses", "name": "Amazon SES", "logo": "email-ses", "note": "No free tier. $0.16 per 1,000 emails on the Essentials plan \u2014 the default for new accounts since 21 July 2026; accounts already sending can switch to a la carte at $0.10 per 1,000. Best for: high-volume sending inside AWS at the lowest per-email price.", "fields": [{"key": "api_key", "label": "API key", "type": "password", "required": true}, {"key": "access_key_id", "label": "Access key ID", "type": "text", "required": true}, {"key": "secret_access_key", "label": "Secret access key", "type": "password", "required": true}, {"key": "region", "label": "Region", "type": "text", "required": true, "placeholder": "us-east-1"}, {"key": "sender_email", "label": "Default sender email", "type": "email", "placeholder": "news@example.com", "help": "From address used when a send does not supply its own."}, {"key": "sender_name", "label": "Default sender name", "type": "text", "placeholder": "Unified"}, {"key": "webhook_secret", "label": "Webhook secret / signing key", "type": "password", "help": "Secret used to verify incoming webhooks from this provider."}, {"key": "test_mode", "label": "Test mode (sandbox)", "type": "checkbox", "help": "When on, mail is recorded as a simulated delivery and never sent."}]}, {"id": "resend", "name": "Resend", "logo": "email-resend", "note": "Free 3,000 emails/month (100/day, verified domain required). Best for: developers sending transactional mail from a modern API.", "fields": [{"key": "api_key", "label": "API key", "type": "password", "required": true}, {"key": "sender_email", "label": "Default sender email", "type": "email", "placeholder": "news@example.com", "help": "From address used when a send does not supply its own."}, {"key": "sender_name", "label": "Default sender name", "type": "text", "placeholder": "Unified"}, {"key": "webhook_secret", "label": "Webhook secret / signing key", "type": "password", "help": "Secret used to verify incoming webhooks from this provider."}, {"key": "test_mode", "label": "Test mode (sandbox)", "type": "checkbox", "help": "When on, mail is recorded as a simulated delivery and never sent."}]}, {"id": "mailchimp", "name": "Mailchimp Transactional (Mandrill)", "logo": "email-mailchimp", "note": "Paid, no free tier. Best for: Mailchimp users who need transactional mail (receipts, password resets) from the same account as their marketing lists.", "fields": [{"key": "api_key", "label": "API key", "type": "password", "required": true}, {"key": "webhook_url", "label": "Webhook URL", "type": "url", "placeholder": "https://unified.example/webhooks/mandrill"}, {"key": "sender_email", "label": "Default sender email", "type": "email", "placeholder": "news@example.com", "help": "From address used when a send does not supply its own."}, {"key": "sender_name", "label": "Default sender name", "type": "text", "placeholder": "Unified"}, {"key": "webhook_secret", "label": "Webhook secret / signing key", "type": "password", "help": "Secret used to verify incoming webhooks from this provider."}, {"key": "test_mode", "label": "Test mode (sandbox)", "type": "checkbox", "help": "When on, mail is recorded as a simulated delivery and never sent."}]}, {"id": "brevo", "name": "Brevo (Sendinblue)", "logo": "email-brevo", "note": "Free 300 emails/day (about 9,000/month). Best for: small senders who want transactional and marketing mail on one free plan.", "fields": [{"key": "api_key", "label": "API key", "type": "password", "required": true}, {"key": "sender_email", "label": "Default sender email", "type": "email", "placeholder": "news@example.com", "help": "From address used when a send does not supply its own."}, {"key": "sender_name", "label": "Default sender name", "type": "text", "placeholder": "Unified"}, {"key": "webhook_secret", "label": "Webhook secret / signing key", "type": "password", "help": "Secret used to verify incoming webhooks from this provider."}, {"key": "test_mode", "label": "Test mode (sandbox)", "type": "checkbox", "help": "When on, mail is recorded as a simulated delivery and never sent."}]}, {"id": "sparkpost", "name": "SparkPost", "logo": "email-sparkpost", "note": "Free 1,000 emails/month (50/day, no credit card) \u2014 now Bird Email. Best for: transactional mail with a generous no-card free tier.", "fields": [{"key": "api_key", "label": "API key", "type": "password", "required": true}, {"key": "region", "label": "Region", "type": "select", "options": [["us", "US"], ["eu", "EU"]]}, {"key": "sender_email", "label": "Default sender email", "type": "email", "placeholder": "news@example.com", "help": "From address used when a send does not supply its own."}, {"key": "sender_name", "label": "Default sender name", "type": "text", "placeholder": "Unified"}, {"key": "webhook_secret", "label": "Webhook secret / signing key", "type": "password", "help": "Secret used to verify incoming webhooks from this provider."}, {"key": "test_mode", "label": "Test mode (sandbox)", "type": "checkbox", "help": "When on, mail is recorded as a simulated delivery and never sent."}]}, /* Task 354: 43 new providers — 26 mailbox SMTP presets, 13 dual API/SMTP relays via SMTP, 4 generic HTTP providers */ {"id": "zoho", "name": "Zoho Mail", "logo": "email-zoho", "note": "Free Forever plan: 1 domain, 5 users, 5 GB per user. Best for: small teams that want a free business mailbox with their own domain.", "fields": [{"key": "username", "label": "SMTP username", "type": "text", "required": true, "placeholder": "you@yourdomain.com", "help": "SMTP username (usually the full email address)."}, {"key": "password", "label": "SMTP password", "type": "password", "required": true, "help": "Zoho account password. Stored encrypted; never shown again."}, {"key": "sender_email", "label": "Default sender email", "type": "email", "required": true, "placeholder": "you@yourdomain.com", "help": "From address used when a send does not supply its own. Must be verified with the provider."}, {"key": "sender_name", "label": "Default sender name", "type": "text", "placeholder": "Example News", "help": "Display name shown next to the sender email."}, {"key": "test_mode", "label": "Test mode (sandbox)", "type": "checkbox", "help": "When on, mail is accepted by the provider sandbox where one exists, otherwise recorded as a simulated delivery and never sent."}]}, {"id": "zoho-eu", "name": "Zoho Mail EU", "logo": "email-zoho-eu", "note": "Free Forever plan: 1 domain, 5 users, 5 GB per user \u2014 same as Zoho Mail, hosted in the EU data center. Best for: small teams in the EU that want a free business mailbox with their own domain.", "fields": [{"key": "username", "label": "SMTP username", "type": "text", "required": true, "placeholder": "you@yourdomain.com", "help": "SMTP username (usually the full email address)."}, {"key": "password", "label": "SMTP password", "type": "password", "required": true, "help": "Zoho account password. Stored encrypted; never shown again."}, {"key": "sender_email", "label": "Default sender email", "type": "email", "required": true, "placeholder": "you@yourdomain.com", "help": "From address used when a send does not supply its own. Must be verified with the provider."}, {"key": "sender_name", "label": "Default sender name", "type": "text", "placeholder": "Example News", "help": "Display name shown next to the sender email."}, {"key": "test_mode", "label": "Test mode (sandbox)", "type": "checkbox", "help": "When on, mail is accepted by the provider sandbox where one exists, otherwise recorded as a simulated delivery and never sent."}]}, {"id": "proton", "name": "Proton Mail", "logo": "email-proton", "note": "Paid account required \u2014 Proton Bridge is not on the free plan. Best for: privacy-first sending from a Proton mailbox.", "fields": [{"key": "username", "label": "SMTP username", "type": "text", "required": true, "placeholder": "you@proton.me", "help": "SMTP username (usually the full email address)."}, {"key": "password", "label": "Proton account password", "type": "password", "required": true, "help": "Proton account password (the Bridge login). Stored encrypted; never shown again."}, {"key": "sender_email", "label": "Default sender email", "type": "email", "required": true, "placeholder": "you@proton.me", "help": "From address used when a send does not supply its own. Must be verified with the provider."}, {"key": "sender_name", "label": "Default sender name", "type": "text", "placeholder": "Example News", "help": "Display name shown next to the sender email."}, {"key": "test_mode", "label": "Test mode (sandbox)", "type": "checkbox", "help": "When on, mail is accepted by the provider sandbox where one exists, otherwise recorded as a simulated delivery and never sent."}]}, {"id": "yandex", "name": "Yandex Mail", "logo": "email-yandex", "note": "Free. Best for: sending from a Yandex mailbox (including Yandex 360 for Business).", "fields": [{"key": "username", "label": "SMTP username", "type": "text", "required": true, "placeholder": "you@yandex.com", "help": "SMTP username (usually the full email address)."}, {"key": "password", "label": "App password", "type": "password", "required": true, "help": "Yandex requires an app password for SMTP (account settings, App passwords). Stored encrypted; never shown again."}, {"key": "sender_email", "label": "Default sender email", "type": "email", "required": true, "placeholder": "you@yandex.com", "help": "From address used when a send does not supply its own. Must be verified with the provider."}, {"key": "sender_name", "label": "Default sender name", "type": "text", "placeholder": "Example News", "help": "Display name shown next to the sender email."}, {"key": "test_mode", "label": "Test mode (sandbox)", "type": "checkbox", "help": "When on, mail is accepted by the provider sandbox where one exists, otherwise recorded as a simulated delivery and never sent."}]}, {"id": "fastmail", "name": "Fastmail", "logo": "email-fastmail", "note": "Paid (30-day free trial). Best for: privacy-focused mailboxes with reliable SMTP.", "fields": [{"key": "username", "label": "SMTP username", "type": "text", "required": true, "placeholder": "you@fastmail.com", "help": "SMTP username (usually the full email address)."}, {"key": "password", "label": "App password", "type": "password", "required": true, "help": "Fastmail requires an app password (Settings, Privacy & Security, App Passwords). Stored encrypted; never shown again."}, {"key": "sender_email", "label": "Default sender email", "type": "email", "required": true, "placeholder": "you@fastmail.com", "help": "From address used when a send does not supply its own. Must be verified with the provider."}, {"key": "sender_name", "label": "Default sender name", "type": "text", "placeholder": "Example News", "help": "Display name shown next to the sender email."}, {"key": "test_mode", "label": "Test mode (sandbox)", "type": "checkbox", "help": "When on, mail is accepted by the provider sandbox where one exists, otherwise recorded as a simulated delivery and never sent."}]}, {"id": "icloud", "name": "iCloud Mail", "logo": "email-icloud", "note": "Free with an Apple ID. Best for: sending from an iCloud mailbox.", "fields": [{"key": "username", "label": "SMTP username", "type": "text", "required": true, "placeholder": "you@icloud.com", "help": "SMTP username (usually the full email address)."}, {"key": "password", "label": "App-specific password", "type": "password", "required": true, "help": "Create one at appleid.apple.com (Sign-In and Security, App-Specific Passwords); your Apple ID password does not work here. Stored encrypted; never shown again."}, {"key": "sender_email", "label": "Default sender email", "type": "email", "required": true, "placeholder": "you@icloud.com", "help": "From address used when a send does not supply its own. Must be verified with the provider."}, {"key": "sender_name", "label": "Default sender name", "type": "text", "placeholder": "Example News", "help": "Display name shown next to the sender email."}, {"key": "test_mode", "label": "Test mode (sandbox)", "type": "checkbox", "help": "When on, mail is accepted by the provider sandbox where one exists, otherwise recorded as a simulated delivery and never sent."}]}, {"id": "gmx", "name": "GMX", "logo": "email-gmx", "note": "Free. Best for: sending from a GMX mailbox.", "fields": [{"key": "username", "label": "SMTP username", "type": "text", "required": true, "placeholder": "you@gmx.com", "help": "SMTP username (usually the full email address)."}, {"key": "password", "label": "SMTP password", "type": "password", "required": true, "help": "SMTP password. Stored encrypted; never shown again."}, {"key": "sender_email", "label": "Default sender email", "type": "email", "required": true, "placeholder": "you@gmx.com", "help": "From address used when a send does not supply its own. Must be verified with the provider."}, {"key": "sender_name", "label": "Default sender name", "type": "text", "placeholder": "Example News", "help": "Display name shown next to the sender email."}, {"key": "test_mode", "label": "Test mode (sandbox)", "type": "checkbox", "help": "When on, mail is accepted by the provider sandbox where one exists, otherwise recorded as a simulated delivery and never sent."}]}, {"id": "mail-com", "name": "Mail.com", "logo": "email-mail-com", "note": "Free. Best for: sending from a mail.com mailbox.", "fields": [{"key": "username", "label": "SMTP username", "type": "text", "required": true, "placeholder": "you@mail.com", "help": "SMTP username (usually the full email address)."}, {"key": "password", "label": "SMTP password", "type": "password", "required": true, "help": "SMTP password. Stored encrypted; never shown again."}, {"key": "sender_email", "label": "Default sender email", "type": "email", "required": true, "placeholder": "you@mail.com", "help": "From address used when a send does not supply its own. Must be verified with the provider."}, {"key": "sender_name", "label": "Default sender name", "type": "text", "placeholder": "Example News", "help": "Display name shown next to the sender email."}, {"key": "test_mode", "label": "Test mode (sandbox)", "type": "checkbox", "help": "When on, mail is accepted by the provider sandbox where one exists, otherwise recorded as a simulated delivery and never sent."}]}, {"id": "aol", "name": "AOL Mail", "logo": "email-aol", "note": "Free. Best for: sending from an AOL mailbox.", "fields": [{"key": "username", "label": "SMTP username", "type": "text", "required": true, "placeholder": "you@aol.com", "help": "SMTP username (usually the full email address)."}, {"key": "password", "label": "App password", "type": "password", "required": true, "help": "AOL requires an app password for SMTP. Stored encrypted; never shown again."}, {"key": "sender_email", "label": "Default sender email", "type": "email", "required": true, "placeholder": "you@aol.com", "help": "From address used when a send does not supply its own. Must be verified with the provider."}, {"key": "sender_name", "label": "Default sender name", "type": "text", "placeholder": "Example News", "help": "Display name shown next to the sender email."}, {"key": "test_mode", "label": "Test mode (sandbox)", "type": "checkbox", "help": "When on, mail is accepted by the provider sandbox where one exists, otherwise recorded as a simulated delivery and never sent."}]}, {"id": "qq-mail", "name": "QQ Mail", "logo": "email-qq-mail", "note": "Free. Best for: sending from a QQ mailbox (China).", "fields": [{"key": "username", "label": "SMTP username", "type": "text", "required": true, "placeholder": "123456789@qq.com", "help": "SMTP username (usually the full email address)."}, {"key": "password", "label": "Authorization code", "type": "password", "required": true, "help": "Generated authorization code from the QQ Mail settings (not the mailbox password). Stored encrypted; never shown again."}, {"key": "sender_email", "label": "Default sender email", "type": "email", "required": true, "placeholder": "123456789@qq.com", "help": "From address used when a send does not supply its own. Must be verified with the provider."}, {"key": "sender_name", "label": "Default sender name", "type": "text", "placeholder": "Example News", "help": "Display name shown next to the sender email."}, {"key": "test_mode", "label": "Test mode (sandbox)", "type": "checkbox", "help": "When on, mail is accepted by the provider sandbox where one exists, otherwise recorded as a simulated delivery and never sent."}]}, {"id": "mail-163", "name": "163 Mail", "logo": "email-mail-163", "note": "Free. Best for: sending from a 163.com (NetEase) mailbox.", "fields": [{"key": "username", "label": "SMTP username", "type": "text", "required": true, "placeholder": "you@163.com", "help": "SMTP username (usually the full email address)."}, {"key": "password", "label": "Authorization code", "type": "password", "required": true, "help": "Generated authorization code from the 163 Mail settings (not the mailbox password). Stored encrypted; never shown again."}, {"key": "sender_email", "label": "Default sender email", "type": "email", "required": true, "placeholder": "you@163.com", "help": "From address used when a send does not supply its own. Must be verified with the provider."}, {"key": "sender_name", "label": "Default sender name", "type": "text", "placeholder": "Example News", "help": "Display name shown next to the sender email."}, {"key": "test_mode", "label": "Test mode (sandbox)", "type": "checkbox", "help": "When on, mail is accepted by the provider sandbox where one exists, otherwise recorded as a simulated delivery and never sent."}]}, {"id": "mail-126", "name": "126 Mail", "logo": "email-mail-126", "note": "Free. Best for: sending from a 126.com (NetEase) mailbox.", "fields": [{"key": "username", "label": "SMTP username", "type": "text", "required": true, "placeholder": "you@126.com", "help": "SMTP username (usually the full email address)."}, {"key": "password", "label": "Authorization code", "type": "password", "required": true, "help": "Generated authorization code from the 126 Mail settings (not the mailbox password). Stored encrypted; never shown again."}, {"key": "sender_email", "label": "Default sender email", "type": "email", "required": true, "placeholder": "you@126.com", "help": "From address used when a send does not supply its own. Must be verified with the provider."}, {"key": "sender_name", "label": "Default sender name", "type": "text", "placeholder": "Example News", "help": "Display name shown next to the sender email."}, {"key": "test_mode", "label": "Test mode (sandbox)", "type": "checkbox", "help": "When on, mail is accepted by the provider sandbox where one exists, otherwise recorded as a simulated delivery and never sent."}]}, {"id": "sina", "name": "Sina Mail", "logo": "email-sina", "note": "Free. Best for: sending from a Sina mailbox.", "fields": [{"key": "username", "label": "SMTP username", "type": "text", "required": true, "placeholder": "you@sina.com", "help": "SMTP username (usually the full email address)."}, {"key": "password", "label": "Authorization code", "type": "password", "required": true, "help": "Generated authorization code from the Sina Mail settings (not the mailbox password). Stored encrypted; never shown again."}, {"key": "sender_email", "label": "Default sender email", "type": "email", "required": true, "placeholder": "you@sina.com", "help": "From address used when a send does not supply its own. Must be verified with the provider."}, {"key": "sender_name", "label": "Default sender name", "type": "text", "placeholder": "Example News", "help": "Display name shown next to the sender email."}, {"key": "test_mode", "label": "Test mode (sandbox)", "type": "checkbox", "help": "When on, mail is accepted by the provider sandbox where one exists, otherwise recorded as a simulated delivery and never sent."}]}, {"id": "mail-ru", "name": "Mail.ru", "logo": "email-mail-ru", "note": "Free. Best for: sending from a Mail.ru mailbox.", "fields": [{"key": "username", "label": "SMTP username", "type": "text", "required": true, "placeholder": "you@mail.ru", "help": "SMTP username (usually the full email address)."}, {"key": "password", "label": "App password", "type": "password", "required": true, "help": "Mail.ru requires an app password for SMTP. Stored encrypted; never shown again."}, {"key": "sender_email", "label": "Default sender email", "type": "email", "required": true, "placeholder": "you@mail.ru", "help": "From address used when a send does not supply its own. Must be verified with the provider."}, {"key": "sender_name", "label": "Default sender name", "type": "text", "placeholder": "Example News", "help": "Display name shown next to the sender email."}, {"key": "test_mode", "label": "Test mode (sandbox)", "type": "checkbox", "help": "When on, mail is accepted by the provider sandbox where one exists, otherwise recorded as a simulated delivery and never sent."}]}, {"id": "posteo", "name": "Posteo", "logo": "email-posteo", "note": "Paid, 1 euro/month. Best for: privacy-first mailboxes with no ads and no tracking.", "fields": [{"key": "username", "label": "SMTP username", "type": "text", "required": true, "placeholder": "you@posteo.de", "help": "SMTP username (usually the full email address)."}, {"key": "password", "label": "SMTP password", "type": "password", "required": true, "help": "SMTP password. Stored encrypted; never shown again."}, {"key": "sender_email", "label": "Default sender email", "type": "email", "required": true, "placeholder": "you@posteo.de", "help": "From address used when a send does not supply its own. Must be verified with the provider."}, {"key": "sender_name", "label": "Default sender name", "type": "text", "placeholder": "Example News", "help": "Display name shown next to the sender email."}, {"key": "test_mode", "label": "Test mode (sandbox)", "type": "checkbox", "help": "When on, mail is accepted by the provider sandbox where one exists, otherwise recorded as a simulated delivery and never sent."}]}, {"id": "migadu", "name": "Migadu", "logo": "email-migadu", "note": "Paid (free trial). Best for: small teams that want unlimited mailboxes per domain at a flat price.", "fields": [{"key": "username", "label": "SMTP username", "type": "text", "required": true, "placeholder": "you@yourdomain.com", "help": "SMTP username (usually the full email address)."}, {"key": "password", "label": "SMTP password", "type": "password", "required": true, "help": "SMTP password. Stored encrypted; never shown again."}, {"key": "sender_email", "label": "Default sender email", "type": "email", "required": true, "placeholder": "you@yourdomain.com", "help": "From address used when a send does not supply its own. Must be verified with the provider."}, {"key": "sender_name", "label": "Default sender name", "type": "text", "placeholder": "Example News", "help": "Display name shown next to the sender email."}, {"key": "test_mode", "label": "Test mode (sandbox)", "type": "checkbox", "help": "When on, mail is accepted by the provider sandbox where one exists, otherwise recorded as a simulated delivery and never sent."}]}, {"id": "mailfence", "name": "Mailfence", "logo": "email-mailfence", "note": "Free 500 MB plan; paid plans for more. Best for: encrypted mailboxes with OpenPGP.", "fields": [{"key": "username", "label": "SMTP username", "type": "text", "required": true, "placeholder": "you@mailfence.com", "help": "SMTP username (usually the full email address)."}, {"key": "password", "label": "SMTP password", "type": "password", "required": true, "help": "SMTP password. Stored encrypted; never shown again."}, {"key": "sender_email", "label": "Default sender email", "type": "email", "required": true, "placeholder": "you@mailfence.com", "help": "From address used when a send does not supply its own. Must be verified with the provider."}, {"key": "sender_name", "label": "Default sender name", "type": "text", "placeholder": "Example News", "help": "Display name shown next to the sender email."}, {"key": "test_mode", "label": "Test mode (sandbox)", "type": "checkbox", "help": "When on, mail is accepted by the provider sandbox where one exists, otherwise recorded as a simulated delivery and never sent."}]}, {"id": "startmail", "name": "StartMail", "logo": "email-startmail", "note": "Paid (free trial). Best for: privacy-first mailboxes with disposable aliases.", "fields": [{"key": "username", "label": "SMTP username", "type": "text", "required": true, "placeholder": "you@startmail.com", "help": "SMTP username (usually the full email address)."}, {"key": "password", "label": "SMTP password", "type": "password", "required": true, "help": "SMTP password. Stored encrypted; never shown again."}, {"key": "sender_email", "label": "Default sender email", "type": "email", "required": true, "placeholder": "you@startmail.com", "help": "From address used when a send does not supply its own. Must be verified with the provider."}, {"key": "sender_name", "label": "Default sender name", "type": "text", "placeholder": "Example News", "help": "Display name shown next to the sender email."}, {"key": "test_mode", "label": "Test mode (sandbox)", "type": "checkbox", "help": "When on, mail is accepted by the provider sandbox where one exists, otherwise recorded as a simulated delivery and never sent."}]}, {"id": "runbox", "name": "Runbox", "logo": "email-runbox", "note": "Paid (free trial). Best for: privacy-focused mailboxes with reliable SMTP.", "fields": [{"key": "username", "label": "SMTP username", "type": "text", "required": true, "placeholder": "you@runbox.com", "help": "SMTP username (usually the full email address)."}, {"key": "password", "label": "SMTP password", "type": "password", "required": true, "help": "SMTP password. Stored encrypted; never shown again."}, {"key": "sender_email", "label": "Default sender email", "type": "email", "required": true, "placeholder": "you@runbox.com", "help": "From address used when a send does not supply its own. Must be verified with the provider."}, {"key": "sender_name", "label": "Default sender name", "type": "text", "placeholder": "Example News", "help": "Display name shown next to the sender email."}, {"key": "test_mode", "label": "Test mode (sandbox)", "type": "checkbox", "help": "When on, mail is accepted by the provider sandbox where one exists, otherwise recorded as a simulated delivery and never sent."}]}, {"id": "kolabnow", "name": "Kolab Now", "logo": "email-kolabnow", "note": "Paid. Best for: privacy-first groupware mailboxes (Kolab).", "fields": [{"key": "username", "label": "SMTP username", "type": "text", "required": true, "placeholder": "you@kolabnow.com", "help": "SMTP username (usually the full email address)."}, {"key": "password", "label": "SMTP password", "type": "password", "required": true, "help": "SMTP password. Stored encrypted; never shown again."}, {"key": "sender_email", "label": "Default sender email", "type": "email", "required": true, "placeholder": "you@kolabnow.com", "help": "From address used when a send does not supply its own. Must be verified with the provider."}, {"key": "sender_name", "label": "Default sender name", "type": "text", "placeholder": "Example News", "help": "Display name shown next to the sender email."}, {"key": "test_mode", "label": "Test mode (sandbox)", "type": "checkbox", "help": "When on, mail is accepted by the provider sandbox where one exists, otherwise recorded as a simulated delivery and never sent."}]}, {"id": "infomaniak", "name": "Infomaniak", "logo": "email-infomaniak", "note": "Paid. Best for: sending from an Infomaniak mailbox (Swiss hosting).", "fields": [{"key": "username", "label": "SMTP username", "type": "text", "required": true, "placeholder": "you@yourdomain.com", "help": "SMTP username (usually the full email address)."}, {"key": "password", "label": "SMTP password", "type": "password", "required": true, "help": "SMTP password. Stored encrypted; never shown again."}, {"key": "sender_email", "label": "Default sender email", "type": "email", "required": true, "placeholder": "you@yourdomain.com", "help": "From address used when a send does not supply its own. Must be verified with the provider."}, {"key": "sender_name", "label": "Default sender name", "type": "text", "placeholder": "Example News", "help": "Display name shown next to the sender email."}, {"key": "test_mode", "label": "Test mode (sandbox)", "type": "checkbox", "help": "When on, mail is accepted by the provider sandbox where one exists, otherwise recorded as a simulated delivery and never sent."}]}, {"id": "ovh", "name": "OVH Mail", "logo": "email-ovh", "note": "Paid, bundled with OVH hosting or a domain. Best for: OVH customers sending from their hosted mailbox.", "fields": [{"key": "username", "label": "SMTP username", "type": "text", "required": true, "placeholder": "you@yourdomain.com", "help": "SMTP username (usually the full email address)."}, {"key": "password", "label": "SMTP password", "type": "password", "required": true, "help": "SMTP password. Stored encrypted; never shown again."}, {"key": "sender_email", "label": "Default sender email", "type": "email", "required": true, "placeholder": "you@yourdomain.com", "help": "From address used when a send does not supply its own. Must be verified with the provider."}, {"key": "sender_name", "label": "Default sender name", "type": "text", "placeholder": "Example News", "help": "Display name shown next to the sender email."}, {"key": "test_mode", "label": "Test mode (sandbox)", "type": "checkbox", "help": "When on, mail is accepted by the provider sandbox where one exists, otherwise recorded as a simulated delivery and never sent."}]}, {"id": "gandi", "name": "Gandi Mail", "logo": "email-gandi", "note": "Paid, bundled with a Gandi domain. Best for: Gandi domain owners sending from their mailbox.", "fields": [{"key": "username", "label": "SMTP username", "type": "text", "required": true, "placeholder": "you@yourdomain.com", "help": "SMTP username (usually the full email address)."}, {"key": "password", "label": "SMTP password", "type": "password", "required": true, "help": "SMTP password. Stored encrypted; never shown again."}, {"key": "sender_email", "label": "Default sender email", "type": "email", "required": true, "placeholder": "you@yourdomain.com", "help": "From address used when a send does not supply its own. Must be verified with the provider."}, {"key": "sender_name", "label": "Default sender name", "type": "text", "placeholder": "Example News", "help": "Display name shown next to the sender email."}, {"key": "test_mode", "label": "Test mode (sandbox)", "type": "checkbox", "help": "When on, mail is accepted by the provider sandbox where one exists, otherwise recorded as a simulated delivery and never sent."}]}, {"id": "namecheap", "name": "Namecheap Private Email", "logo": "email-namecheap", "note": "Paid (Private Email). Best for: Namecheap domain owners sending from their mailbox.", "fields": [{"key": "username", "label": "SMTP username", "type": "text", "required": true, "placeholder": "you@yourdomain.com", "help": "SMTP username (usually the full email address)."}, {"key": "password", "label": "SMTP password", "type": "password", "required": true, "help": "SMTP password. Stored encrypted; never shown again."}, {"key": "sender_email", "label": "Default sender email", "type": "email", "required": true, "placeholder": "you@yourdomain.com", "help": "From address used when a send does not supply its own. Must be verified with the provider."}, {"key": "sender_name", "label": "Default sender name", "type": "text", "placeholder": "Example News", "help": "Display name shown next to the sender email."}, {"key": "test_mode", "label": "Test mode (sandbox)", "type": "checkbox", "help": "When on, mail is accepted by the provider sandbox where one exists, otherwise recorded as a simulated delivery and never sent."}]}, {"id": "dreamhost", "name": "DreamHost Mail", "logo": "email-dreamhost", "note": "Paid, bundled with DreamHost hosting. Best for: DreamHost customers sending from their hosted mailbox.", "fields": [{"key": "username", "label": "SMTP username", "type": "text", "required": true, "placeholder": "you@yourdomain.com", "help": "SMTP username (usually the full email address)."}, {"key": "password", "label": "SMTP password", "type": "password", "required": true, "help": "SMTP password. Stored encrypted; never shown again."}, {"key": "sender_email", "label": "Default sender email", "type": "email", "required": true, "placeholder": "you@yourdomain.com", "help": "From address used when a send does not supply its own. Must be verified with the provider."}, {"key": "sender_name", "label": "Default sender name", "type": "text", "placeholder": "Example News", "help": "Display name shown next to the sender email."}, {"key": "test_mode", "label": "Test mode (sandbox)", "type": "checkbox", "help": "When on, mail is accepted by the provider sandbox where one exists, otherwise recorded as a simulated delivery and never sent."}]}, {"id": "mxroute", "name": "MXroute", "logo": "email-mxroute", "note": "Paid (host per server). Best for: high-volume outbound mail from a dedicated outbound host.", "fields": [{"key": "username", "label": "SMTP username", "type": "text", "required": true, "placeholder": "you@yourdomain.com", "help": "SMTP username (usually the full email address)."}, {"key": "password", "label": "SMTP password", "type": "password", "required": true, "help": "SMTP password. Stored encrypted; never shown again."}, {"key": "sender_email", "label": "Default sender email", "type": "email", "required": true, "placeholder": "you@yourdomain.com", "help": "From address used when a send does not supply its own. Must be verified with the provider."}, {"key": "sender_name", "label": "Default sender name", "type": "text", "placeholder": "Example News", "help": "Display name shown next to the sender email."}, {"key": "test_mode", "label": "Test mode (sandbox)", "type": "checkbox", "help": "When on, mail is accepted by the provider sandbox where one exists, otherwise recorded as a simulated delivery and never sent."}]}, {"id": "sender-net", "name": "Sender.net", "logo": "email-sender-net", "note": "Free 15,000 emails/month (max 2,500 contacts) \u2014 the largest free tier in this catalog. Best for: small senders who want the most free sends.", "fields": [{"key": "username", "label": "SMTP username", "type": "text", "required": true, "placeholder": "you@example.com", "help": "SMTP username from the Sender.net dashboard."}, {"key": "password", "label": "SMTP password", "type": "password", "required": true, "help": "SMTP password from the Sender.net dashboard. Stored encrypted; never shown again."}, {"key": "sender_email", "label": "Default sender email", "type": "email", "required": true, "placeholder": "news@example.com", "help": "From address used when a send does not supply its own. Must be verified with the provider."}, {"key": "sender_name", "label": "Default sender name", "type": "text", "placeholder": "Example News", "help": "Display name shown next to the sender email."}, {"key": "test_mode", "label": "Test mode (sandbox)", "type": "checkbox", "help": "When on, mail is accepted by the provider sandbox where one exists, otherwise recorded as a simulated delivery and never sent."}]}, {"id": "sendpulse", "name": "SendPulse", "logo": "email-sendpulse", "note": "Free 12,000 emails/month. Best for: transactional and marketing mail on one free plan.", "fields": [{"key": "username", "label": "SMTP username", "type": "text", "required": true, "placeholder": "you@example.com", "help": "SMTP username from the SendPulse dashboard (SMTP settings)."}, {"key": "password", "label": "SMTP password", "type": "password", "required": true, "help": "SMTP password from the SendPulse dashboard. Stored encrypted; never shown again."}, {"key": "sender_email", "label": "Default sender email", "type": "email", "required": true, "placeholder": "news@example.com", "help": "From address used when a send does not supply its own. Must be verified with the provider."}, {"key": "sender_name", "label": "Default sender name", "type": "text", "placeholder": "Example News", "help": "Display name shown next to the sender email."}, {"key": "test_mode", "label": "Test mode (sandbox)", "type": "checkbox", "help": "When on, mail is accepted by the provider sandbox where one exists, otherwise recorded as a simulated delivery and never sent."}]}, {"id": "mailjet", "name": "Mailjet", "logo": "email-mailjet", "note": "Free 6,000 emails/month (200/day). Best for: transactional mail with a generous free tier and EU data residency.", "fields": [{"key": "username", "label": "API key (SMTP username)", "type": "text", "required": true, "placeholder": "you@example.com", "help": "Mailjet API key \u2014 it is the SMTP username."}, {"key": "password", "label": "Secret key (SMTP password)", "type": "password", "required": true, "help": "Mailjet secret key \u2014 it is the SMTP password. Stored encrypted; never shown again."}, {"key": "sender_email", "label": "Default sender email", "type": "email", "required": true, "placeholder": "news@example.com", "help": "From address used when a send does not supply its own. Must be verified with the provider."}, {"key": "sender_name", "label": "Default sender name", "type": "text", "placeholder": "Example News", "help": "Display name shown next to the sender email."}, {"key": "test_mode", "label": "Test mode (sandbox)", "type": "checkbox", "help": "When on, mail is accepted by the provider sandbox where one exists, otherwise recorded as a simulated delivery and never sent."}]}, {"id": "mailtrap", "name": "Mailtrap", "logo": "email-mailtrap", "note": "Free 4,000 emails/month. Best for: testing mail safely (sandbox inboxes) and sending production mail from the same account.", "fields": [{"key": "username", "label": "SMTP username", "type": "text", "required": true, "placeholder": "you@example.com", "help": "SMTP username from the Mailtrap inbox (production sending)."}, {"key": "password", "label": "SMTP password", "type": "password", "required": true, "help": "SMTP password from the Mailtrap inbox. Stored encrypted; never shown again."}, {"key": "sender_email", "label": "Default sender email", "type": "email", "required": true, "placeholder": "news@example.com", "help": "From address used when a send does not supply its own. Must be verified with the provider."}, {"key": "sender_name", "label": "Default sender name", "type": "text", "placeholder": "Example News", "help": "Display name shown next to the sender email."}, {"key": "test_mode", "label": "Test mode (sandbox)", "type": "checkbox", "help": "When on, mail is accepted by the provider sandbox where one exists, otherwise recorded as a simulated delivery and never sent."}]}, {"id": "elastic-email", "name": "Elastic Email", "logo": "email-elastic-email", "note": "Free 3,000 emails/month (100/day, no credit card). Best for: low-volume senders who want a free tier without a card.", "fields": [{"key": "username", "label": "SMTP username", "type": "text", "required": true, "placeholder": "you@example.com", "help": "Elastic Email account email \u2014 it is the SMTP username."}, {"key": "password", "label": "API key (SMTP password)", "type": "password", "required": true, "help": "Elastic Email API key \u2014 it is the SMTP password. Stored encrypted; never shown again."}, {"key": "sender_email", "label": "Default sender email", "type": "email", "required": true, "placeholder": "news@example.com", "help": "From address used when a send does not supply its own. Must be verified with the provider."}, {"key": "sender_name", "label": "Default sender name", "type": "text", "placeholder": "Example News", "help": "Display name shown next to the sender email."}, {"key": "test_mode", "label": "Test mode (sandbox)", "type": "checkbox", "help": "When on, mail is accepted by the provider sandbox where one exists, otherwise recorded as a simulated delivery and never sent."}]}, {"id": "smtp2go", "name": "SMTP2GO", "logo": "email-smtp2go", "note": "Free about 1,000 emails/month. Best for: reliable transactional mail with a small free tier.", "fields": [{"key": "username", "label": "SMTP username", "type": "text", "required": true, "placeholder": "you@example.com", "help": "SMTP username from the SMTP2GO dashboard."}, {"key": "password", "label": "SMTP password", "type": "password", "required": true, "help": "SMTP password from the SMTP2GO dashboard. Stored encrypted; never shown again."}, {"key": "sender_email", "label": "Default sender email", "type": "email", "required": true, "placeholder": "news@example.com", "help": "From address used when a send does not supply its own. Must be verified with the provider."}, {"key": "sender_name", "label": "Default sender name", "type": "text", "placeholder": "Example News", "help": "Display name shown next to the sender email."}, {"key": "test_mode", "label": "Test mode (sandbox)", "type": "checkbox", "help": "When on, mail is accepted by the provider sandbox where one exists, otherwise recorded as a simulated delivery and never sent."}]}, {"id": "mailersend", "name": "MailerSend", "logo": "email-mailersend", "note": "Free 500 emails/month (100 API requests/day); a credit card is REQUIRED even for the free tier. Best for: transactional mail from the MailerLite team.", "fields": [{"key": "username", "label": "SMTP username", "type": "text", "required": true, "placeholder": "you@example.com", "help": "SMTP username from the MailerSend dashboard (SMTP settings)."}, {"key": "password", "label": "SMTP password", "type": "password", "required": true, "help": "SMTP password from the MailerSend dashboard. Stored encrypted; never shown again."}, {"key": "sender_email", "label": "Default sender email", "type": "email", "required": true, "placeholder": "news@example.com", "help": "From address used when a send does not supply its own. Must be verified with the provider."}, {"key": "sender_name", "label": "Default sender name", "type": "text", "placeholder": "Example News", "help": "Display name shown next to the sender email."}, {"key": "test_mode", "label": "Test mode (sandbox)", "type": "checkbox", "help": "When on, mail is accepted by the provider sandbox where one exists, otherwise recorded as a simulated delivery and never sent."}]}, {"id": "zeptomail", "name": "ZeptoMail", "logo": "email-zeptomail", "note": "First 10,000 emails free (credit valid 6 months), then $2.50 per 10,000. Transactional only. Best for: cheap high-volume transactional mail from Zoho.", "fields": [{"key": "username", "label": "SMTP username", "type": "text", "required": true, "placeholder": "you@example.com", "help": "ZeptoMail email address \u2014 it is the SMTP username."}, {"key": "password", "label": "Mail Agent password", "type": "password", "required": true, "help": "Password generated in the ZeptoMail Mail Agent. Stored encrypted; never shown again."}, {"key": "sender_email", "label": "Default sender email", "type": "email", "required": true, "placeholder": "news@example.com", "help": "From address used when a send does not supply its own. Must be verified with the provider."}, {"key": "sender_name", "label": "Default sender name", "type": "text", "placeholder": "Example News", "help": "Display name shown next to the sender email."}, {"key": "test_mode", "label": "Test mode (sandbox)", "type": "checkbox", "help": "When on, mail is accepted by the provider sandbox where one exists, otherwise recorded as a simulated delivery and never sent."}]}, {"id": "sendlayer", "name": "SendLayer", "logo": "email-sendlayer", "note": "Trial only (about 200 emails), no free plan. Best for: trying the service before paying.", "fields": [{"key": "username", "label": "SMTP username", "type": "text", "required": true, "placeholder": "you@example.com", "help": "SMTP username from the SendLayer dashboard."}, {"key": "password", "label": "SMTP password", "type": "password", "required": true, "help": "SMTP password from the SendLayer dashboard. Stored encrypted; never shown again."}, {"key": "sender_email", "label": "Default sender email", "type": "email", "required": true, "placeholder": "news@example.com", "help": "From address used when a send does not supply its own. Must be verified with the provider."}, {"key": "sender_name", "label": "Default sender name", "type": "text", "placeholder": "Example News", "help": "Display name shown next to the sender email."}, {"key": "test_mode", "label": "Test mode (sandbox)", "type": "checkbox", "help": "When on, mail is accepted by the provider sandbox where one exists, otherwise recorded as a simulated delivery and never sent."}]}, {"id": "socketlabs", "name": "SocketLabs", "logo": "email-socketlabs", "note": "No free plan \u2014 first month free (40,000 emails), then from $39.95/month. Best for: high-volume senders who want a dedicated sending platform.", "fields": [{"key": "username", "label": "Server ID (SMTP username)", "type": "text", "required": true, "placeholder": "you@example.com", "help": "SocketLabs Server ID \u2014 it is the SMTP username."}, {"key": "password", "label": "API key (SMTP password)", "type": "password", "required": true, "help": "SocketLabs API key \u2014 it is the SMTP password. Stored encrypted; never shown again."}, {"key": "sender_email", "label": "Default sender email", "type": "email", "required": true, "placeholder": "news@example.com", "help": "From address used when a send does not supply its own. Must be verified with the provider."}, {"key": "sender_name", "label": "Default sender name", "type": "text", "placeholder": "Example News", "help": "Display name shown next to the sender email."}, {"key": "test_mode", "label": "Test mode (sandbox)", "type": "checkbox", "help": "When on, mail is accepted by the provider sandbox where one exists, otherwise recorded as a simulated delivery and never sent."}]}, {"id": "smtp-com", "name": "SMTP.com", "logo": "email-smtp-com", "note": "Paid from $25 per 50,000 emails. Best for: high-volume transactional mail with a simple SMTP relay.", "fields": [{"key": "username", "label": "SMTP username", "type": "text", "required": true, "placeholder": "you@example.com", "help": "SMTP username from the SMTP.com dashboard."}, {"key": "password", "label": "SMTP password", "type": "password", "required": true, "help": "SMTP password from the SMTP.com dashboard. Stored encrypted; never shown again."}, {"key": "sender_email", "label": "Default sender email", "type": "email", "required": true, "placeholder": "news@example.com", "help": "From address used when a send does not supply its own. Must be verified with the provider."}, {"key": "sender_name", "label": "Default sender name", "type": "text", "placeholder": "Example News", "help": "Display name shown next to the sender email."}, {"key": "test_mode", "label": "Test mode (sandbox)", "type": "checkbox", "help": "When on, mail is accepted by the provider sandbox where one exists, otherwise recorded as a simulated delivery and never sent."}]}, {"id": "campaign-monitor", "name": "Campaign Monitor", "logo": "email-campaign-monitor", "note": "Trial only, no free plan (marketing API); the transactional SMTP relay is paid. Best for: sending through Campaign Monitor's transactional relay with an existing account.", "fields": [{"key": "username", "label": "SMTP username (API token)", "type": "text", "required": true, "help": "Campaign Monitor Transactional SMTP token \u2014 the same token goes in both the username and the password fields."}, {"key": "password", "label": "SMTP password (same API token)", "type": "password", "required": true, "help": "The same Campaign Monitor Transactional SMTP token as the username. Stored encrypted; never shown again."}, {"key": "sender_email", "label": "Default sender email", "type": "email", "required": true, "placeholder": "news@example.com", "help": "From address used when a send does not supply its own. Must be verified with the provider."}, {"key": "sender_name", "label": "Default sender name", "type": "text", "placeholder": "Example News", "help": "Display name shown next to the sender email."}, {"key": "test_mode", "label": "Test mode (sandbox)", "type": "checkbox", "help": "When on, mail is accepted by the provider sandbox where one exists, otherwise recorded as a simulated delivery and never sent."}]}, {"id": "plunk", "name": "Plunk", "logo": "email-plunk", "note": "Free plan exists (check the provider site for the current limit). Best for: SaaS transactional mail from an open-source platform (self-hostable).", "fields": [{"key": "api_key", "label": "Secret key", "type": "password", "required": true, "help": "Plunk secret key (sk_...) from Settings, API Keys. Sent as a Bearer token."}, {"key": "sender_email", "label": "Default sender email", "type": "email", "required": true, "placeholder": "news@example.com", "help": "From address used when a send does not supply its own. Must be verified with the provider."}, {"key": "sender_name", "label": "Default sender name", "type": "text", "placeholder": "Example News", "help": "Display name shown next to the sender email."}, {"key": "webhook_secret", "label": "Webhook secret / signing key", "type": "password", "help": "Secret used to verify incoming webhooks from this provider (see the webhook notes for the kind)."}, {"key": "test_mode", "label": "Test mode (sandbox)", "type": "checkbox", "help": "When on, mail is accepted by the provider sandbox where one exists, otherwise recorded as a simulated delivery and never sent."}]}, {"id": "courier", "name": "Courier", "logo": "email-courier", "note": "Free 10,000 sends/month (per third-party comparison Nov 2026 \u2014 check the provider site). Best for: routing one notification to email, SMS and push from a single API.", "fields": [{"key": "api_key", "label": "API key", "type": "password", "required": true, "help": "Courier API key from Settings, API Keys. Sent as a Bearer token."}, {"key": "sender_email", "label": "Default sender email", "type": "email", "required": true, "placeholder": "news@example.com", "help": "From address used when a send does not supply its own. Must be verified with the provider."}, {"key": "sender_name", "label": "Default sender name", "type": "text", "placeholder": "Example News", "help": "Display name shown next to the sender email."}, {"key": "webhook_secret", "label": "Webhook secret / signing key", "type": "password", "help": "Secret used to verify incoming webhooks from this provider (see the webhook notes for the kind)."}, {"key": "test_mode", "label": "Test mode (sandbox)", "type": "checkbox", "help": "When on, mail is accepted by the provider sandbox where one exists, otherwise recorded as a simulated delivery and never sent."}]}, {"id": "loops", "name": "Loops", "logo": "email-loops", "note": "Free plan: up to 4,000 sends/month to the first 1,000 contacts. Best for: SaaS lifecycle and transactional mail from templates.", "fields": [{"key": "api_key", "label": "API key", "type": "password", "required": true, "help": "Loops API key from Settings, API. Sent as a Bearer token."}, {"key": "sender_email", "label": "Default sender email", "type": "email", "required": true, "placeholder": "news@example.com", "help": "From address used when a send does not supply its own. Must be verified with the provider."}, {"key": "sender_name", "label": "Default sender name", "type": "text", "placeholder": "Example News", "help": "Display name shown next to the sender email."}, {"key": "webhook_secret", "label": "Webhook secret / signing key", "type": "password", "help": "Secret used to verify incoming webhooks from this provider (see the webhook notes for the kind)."}, {"key": "test_mode", "label": "Test mode (sandbox)", "type": "checkbox", "help": "When on, mail is accepted by the provider sandbox where one exists, otherwise recorded as a simulated delivery and never sent."}]}, {"id": "onesignal", "name": "OneSignal Email", "logo": "email-onesignal", "note": "Free 10,000 emails/month. Best for: apps already using OneSignal for push that want email from the same account.", "fields": [{"key": "api_key", "label": "REST API key", "type": "password", "required": true, "help": "OneSignal REST API key from Settings, Keys & IDs. Sent as a Basic token."}, {"key": "domain", "label": "OneSignal App ID", "type": "text", "required": true, "help": "Your OneSignal App ID (Settings, Keys & IDs); used as app_id in the API payload. Stored in the domain field."}, {"key": "sender_email", "label": "Default sender email", "type": "email", "required": true, "placeholder": "news@example.com", "help": "From address used when a send does not supply its own. Must be verified with the provider."}, {"key": "sender_name", "label": "Default sender name", "type": "text", "placeholder": "Example News", "help": "Display name shown next to the sender email."}, {"key": "webhook_secret", "label": "Webhook secret / signing key", "type": "password", "help": "Secret used to verify incoming webhooks from this provider (see the webhook notes for the kind)."}, {"key": "test_mode", "label": "Test mode (sandbox)", "type": "checkbox", "help": "When on, mail is accepted by the provider sandbox where one exists, otherwise recorded as a simulated delivery and never sent."}]}];

  /* The default builtin: local sendmail. */
  var selectedMailProvider = "sendmail";

  function renderTopbar(page, layout) {
    var nav = "";
    if (layout === "app" || layout === "public") {
      nav = '<nav class="topbar-nav" aria-label="Primary">' +
        NAV.map(function (item) {
          var active = page === item.page || (page === "module" && item.page === "modules");
          return '<a href="' + item.href + '"' +
            (active ? ' class="is-active" aria-current="page"' : "") +
            ">" + item.label + "</a>";
        }).join("") +
        "</nav>";
    }
    var right =
      '<div class="topbar-right">' +
      '<div class="theme-switcher" role="group" aria-label="Demo theme">' +
      '<button type="button" data-theme-switch="default" aria-pressed="true">Default</button>' +
      '<button type="button" data-theme-switch="editorial" aria-pressed="false">Editorial Dark</button>' +
      "</div>";
    if (layout === "app") {
      right +=
        '<button type="button" class="cart-button" data-cart-button aria-label="Demo cart">' +
        'Cart <span class="cart-count" id="cart-count">0</span></button>' +
        '<a class="user-chip" href="admin.html"><span class="avatar">DA</span><span>Demo Admin</span></a>';
    } else {
      right += '<a class="button button-small" href="login.html">Sign in</a>';
    }
    right += "</div>";
    return (
      '<header class="topbar">' +
      '<a class="brand" href="index.html"><span class="brand-mark" aria-hidden="true">U</span>' +
      '<span class="brand-name">Unified</span> <span class="demo-badge">Demo</span></a>' +
      nav + right +
      "</header>"
    );
  }

  function renderSidebar(page) {
    var links = NAV.map(function (item) {
      var active = page === item.page || (page === "module" && item.page === "modules");
      return '<a class="sidebar-link' + (active ? " is-active" : "") + '" href="' + item.href + '"' +
        (active ? ' aria-current="page"' : "") + ">" + item.label + "</a>";
    }).join("");
    var modules = SIDEBAR_MODULES.map(function (mod) {
      return '<a class="sidebar-link" href="' + mod.href + '">' + icon(mod.id) +
        "<span>" + mod.label + "</span></a>";
    }).join("");
    return (
      '<aside class="sidebar" aria-label="Demo navigation">' +
      '<p class="sidebar-section">Navigate</p>' + links +
      '<p class="sidebar-section">Modules</p>' + modules +
      "</aside>"
    );
  }

  function renderFooter() {
    return (
      '<footer class="footer">' +
      "<p><strong>Unified demo</strong> — static preview, no backend. " +
      "All data on this page is demo data; nothing is saved or sent anywhere.</p>" +
      '<p><a href="index.html">Home</a> &middot; <a href="dashboard.html">Dashboard</a> &middot; <a href="tenant.html">Tenant demo</a> &middot; <a href="modules.html">Modules</a> &middot; ' +
      '<a href="store.html">Store</a> &middot; <a href="settings.html">Settings</a></p>' +
      "</footer>"
    );
  }

  function renderShell() {
    /* Icon sprite (real sprite, injected so <use> works from file:// too). */
    if (window.UNIFIED_DEMO_SPRITE) {
      var sprite = document.createElement("div");
      sprite.setAttribute("aria-hidden", "true");
      sprite.style.display = "none";
      sprite.innerHTML = window.UNIFIED_DEMO_SPRITE;
      document.body.insertBefore(sprite, document.body.firstChild);
    }

    var main = document.querySelector("main");
    if (!main) return;
    var page = document.body.getAttribute("data-page") || "dashboard";
    var layout = document.body.getAttribute("data-layout") || "app";

    var header = document.createElement("div");
    header.innerHTML = renderTopbar(page, layout);
    document.body.insertBefore(header.firstChild, main);

    if (layout === "app") {
      var shell = document.createElement("div");
      shell.className = "shell";
      main.parentNode.insertBefore(shell, main);
      var aside = document.createElement("div");
      aside.innerHTML = renderSidebar(page);
      shell.appendChild(aside.firstChild);
      shell.appendChild(main);
    }

    var footer = document.createElement("div");
    footer.innerHTML = renderFooter();
    document.body.appendChild(footer.firstChild);

    var toast = document.createElement("div");
    toast.className = "toast";
    toast.id = "demo-toast";
    toast.setAttribute("role", "status");
    document.body.appendChild(toast);
  }

  /* ---------- Toast ---------- */

  var toastTimer = null;

  function showToast(message) {
    var toast = document.getElementById("demo-toast");
    if (!toast) return;
    toast.textContent = message;
    toast.classList.add("show");
    if (toastTimer) clearTimeout(toastTimer);
    toastTimer = setTimeout(function () {
      toast.classList.remove("show");
    }, 2600);
  }

  /* ---------- Demo interactions ---------- */

  function initTheme() {
    applyTheme(currentTheme());
    document.addEventListener("click", function (event) {
      var button = event.target.closest("[data-theme-switch], [data-theme-pick]");
      if (!button) return;
      applyTheme(button.getAttribute("data-theme-switch") || button.getAttribute("data-theme-pick"));
      showToast("Theme switched (demo only).");
    });
  }

  function initTicker() {
    var item = document.querySelector("[data-ticker-item]");
    var items = document.querySelectorAll("[data-ticker-source] li");
    if (!item || items.length === 0) return;
    var index = 0;
    setInterval(function () {
      index = (index + 1) % items.length;
      item.style.opacity = "0";
      setTimeout(function () {
        item.textContent = items[index].textContent;
        item.style.opacity = "1";
      }, 300);
    }, 5000);
  }

  function initModuleFilter() {
    var input = document.getElementById("module-search");
    if (!input) return;
    var cards = document.querySelectorAll("[data-module-name]");
    var empty = document.getElementById("module-empty");
    input.addEventListener("input", function () {
      var query = input.value.trim().toLowerCase();
      var visible = 0;
      cards.forEach(function (card) {
        var match = card.getAttribute("data-module-name").toLowerCase().indexOf(query) !== -1;
        card.style.display = match ? "" : "none";
        if (match) visible += 1;
      });
      if (empty) empty.hidden = visible !== 0;
    });
  }

  function initToggles() {
    document.addEventListener("click", function (event) {
      var toggle = event.target.closest(".switch");
      if (!toggle) return;
      var on = toggle.getAttribute("aria-pressed") !== "true";
      toggle.setAttribute("aria-pressed", on ? "true" : "false");
      showToast(toggle.getAttribute("data-label") + (on ? " enabled (demo)." : " disabled (demo)."));
    });
  }

  var cartCount = 0;

  function initCart() {
    document.addEventListener("click", function (event) {
      var button = event.target.closest("[data-add-cart]");
      if (!button) return;
      cartCount += 1;
      var badge = document.getElementById("cart-count");
      if (badge) badge.textContent = String(cartCount);
      showToast(button.getAttribute("data-add-cart") + " added to cart (demo).");
    });
    var cartButton = document.querySelector("[data-cart-button]");
    if (cartButton) {
      cartButton.addEventListener("click", function () {
        showToast(
          cartCount === 0
            ? "Your demo cart is empty."
            : "Demo cart: " + cartCount + " item" + (cartCount === 1 ? "" : "s") + " (no checkout in this demo)."
        );
      });
    }
  }

  function initTabs() {
    var buttons = document.querySelectorAll("[data-tab]");
    if (buttons.length === 0) return;
    buttons.forEach(function (button) {
      button.addEventListener("click", function () {
        var target = button.getAttribute("data-tab");
        buttons.forEach(function (other) {
          other.setAttribute("aria-selected", other === button ? "true" : "false");
        });
        document.querySelectorAll("[data-tab-panel]").forEach(function (panel) {
          panel.hidden = panel.getAttribute("data-tab-panel") !== target;
        });
      });
    });
  }

  function initForms() {
    document.querySelectorAll("form[data-demo-form]").forEach(function (form) {
      form.addEventListener("submit", function (event) {
        event.preventDefault();
        showToast("Demo only — nothing was saved.");
        var redirect = form.getAttribute("data-demo-redirect");
        if (redirect) {
          setTimeout(function () {
            window.location.href = redirect;
          }, 700);
        }
      });
    });
    var totpToggle = document.getElementById("login-show-totp");
    var totpField = document.getElementById("login-totp-field");
    if (totpToggle && totpField) {
      totpToggle.addEventListener("click", function () {
        var showing = totpField.hidden;
        totpField.hidden = !showing;
        totpToggle.textContent = showing ? "Hide authenticator code" : "Use an authenticator code";
        if (showing) document.getElementById("login-totp").focus();
      });
    }
  }

  /* Generic demo buttons: <button data-toast="message"> shows a toast. */
  function initToastButtons() {
    document.addEventListener("click", function (event) {
      var button = event.target.closest("[data-toast]");
      if (button) showToast(button.getAttribute("data-toast"));
    });
  }


  /* ---------- Site settings (task 351) ---------- */

  function formatDemoDate(date, format) {
    var months = ["January", "February", "March", "April", "May", "June",
      "July", "August", "September", "October", "November", "December"];
    var y = date.getFullYear(), m = date.getMonth() + 1, d = date.getDate();
    var pad = function (n) { return (n < 10 ? "0" : "") + n; };
    if (format === "Y-m-d") return y + "-" + pad(m) + "-" + pad(d);
    if (format === "m/d/Y") return pad(m) + "/" + pad(d) + "/" + y;
    if (format === "d/m/Y") return pad(d) + "/" + pad(m) + "/" + y;
    return months[date.getMonth()] + " " + d + ", " + y; /* "F j, Y" */
  }

  function formatDemoTime(date, format) {
    var h = date.getHours(), m = date.getMinutes();
    var pad = function (n) { return (n < 10 ? "0" : "") + n; };
    if (format === "H:i") return pad(h) + ":" + pad(m);
    var h12 = h % 12 || 12, suffix = h < 12 ? "am" : "pm";
    if (format === "g:i A") return h12 + ":" + pad(m) + " " + suffix.toUpperCase();
    return h12 + ":" + pad(m) + " " + suffix;
  }

  function updateDateTimePreview() {
    var dateSelect = document.querySelector("[data-date-format]");
    var timeSelect = document.querySelector("[data-time-format]");
    var datePreview = document.querySelector("[data-date-preview]");
    var timePreview = document.querySelector("[data-time-preview]");
    if (!dateSelect || !timeSelect || !datePreview || !timePreview) return;
    var now = new Date();
    datePreview.textContent = formatDemoDate(now, dateSelect.value);
    timePreview.textContent = formatDemoTime(now, timeSelect.value);
  }

  function initSiteSettings() {
    var tz = document.getElementById("settings-timezone");
    if (tz) {
      tz.innerHTML = IANA_TIMEZONES.map(function (zone) {
        return '<option value="' + zone + '">' + zone + "</option>";
      }).join("");
      tz.value = "Asia/Jakarta";
    }
    updateDateTimePreview();
    document.addEventListener("change", function (event) {
      if (event.target.matches("[data-date-format], [data-time-format]")) {
        updateDateTimePreview();
      }
    });
  }

  /* ---------- Mail provider picker (task 350) ---------- */

  function mailFieldHtml(provider, field) {
    var id = "mail-" + provider.id + "-" + field.key;
    if (field.type === "checkbox") {
      return '<div class="check-row"><div><strong>' + field.label +
        "</strong><p>" + (field.help || "") + '</p></div>' +
        '<button type="button" class="switch" aria-pressed="false" data-label="' +
        field.label + '" aria-label="Toggle ' + field.label + '"></button></div>';
    }
    var label = '<label for="' + id + '">' + field.label +
      (field.required ? " *" : "") + "</label>";
    var input;
    if (field.type === "select") {
      input = '<select id="' + id + '" name="' + field.key + '">' +
        field.options.map(function (opt) {
          return '<option value="' + opt[0] + '">' + opt[1] + "</option>";
        }).join("") + "</select>";
    } else {
      input = '<input id="' + id + '" name="' + field.key + '" type="' + field.type + '"' +
        (field.placeholder ? ' placeholder="' + field.placeholder + '"' : "") +
        (field.required ? " required" : "") + ">";
    }
    return '<div class="field">' + label + input +
      (field.help ? '<p class="field-help">' + field.help + "</p>" : "") + "</div>";
  }

  function renderMailProviderGrid() {
    var grid = document.getElementById("mail-provider-grid");
    if (!grid) return;
    grid.innerHTML = DEMO_MAIL_PROVIDERS.map(function (provider) {
      var selected = provider.id === selectedMailProvider;
      return '<button type="button" class="provider-card' +
        (selected ? " is-selected" : "") + '" data-provider="' + provider.id +
        '" role="radio" aria-checked="' + selected + '">' +
        '<img class="provider-logo" src="assets/logos/' + provider.logo +
        '.svg" alt="" width="24" height="24">' +
        '<span class="provider-name">' + provider.name + "</span>" +
        '<span class="provider-note">' + provider.note + "</span>" +
        "</button>";
    }).join("");
  }

  function renderMailProviderFields() {
    var provider = null;
    DEMO_MAIL_PROVIDERS.forEach(function (candidate) {
      if (candidate.id === selectedMailProvider) provider = candidate;
    });
    var container = document.getElementById("mail-provider-fields");
    if (!provider || !container) return;
    var html = "";
    if (provider.oauth) {
      html += '<p><button type="button" class="button button-secondary" data-toast="OAuth connect is demo-only — the real flow opens ' +
        provider.oauth + ' in a browser.">Connect with ' + provider.oauth +
        '</button> <span class="muted">No account linked yet (demo).</span></p>';
    }
    provider.fields.forEach(function (field) {
      html += mailFieldHtml(provider, field);
    });
    container.innerHTML = html;
  }

  function initMailProviderPicker() {
    renderMailProviderGrid();
    renderMailProviderFields();
    document.addEventListener("click", function (event) {
      var card = event.target.closest("[data-provider]");
      if (card) {
        selectedMailProvider = card.getAttribute("data-provider");
        renderMailProviderGrid();
        renderMailProviderFields();
        return;
      }
      var button = event.target.closest("[data-mail-test-connection]");
      if (!button) return;
      var provider = null;
      DEMO_MAIL_PROVIDERS.forEach(function (candidate) {
        if (candidate.id === selectedMailProvider) provider = candidate;
      });
      showToast("Connection test passed for " + provider.name + " (demo).");
    });
  }

  /* ---------- Boot ---------- */

  document.addEventListener("DOMContentLoaded", function () {
    renderShell();
    initTheme();
    initTicker();
    initModuleFilter();
    initToggles();
    initCart();
    initTabs();
    initSiteSettings();
    initMailProviderPicker();
    initToastButtons();
    initForms();
  });
})();

/* ==========================================================================
   Widgets — task 352

   Demo widget system: every section marked [data-widget] is a widget.
   Pin, add (from the picker), remove and drag-to-reorder (Pointer
   Events only — never HTML5 drag-and-drop). The layout is stored in
   localStorage, per page, browser only; the real app stores layouts
   in the widget_layouts table (migration 000129).

   Pinned widgets sort first, always: a pinned widget dragged below
   an unpinned one moves back to the top on drop. Unpin it to move
   it down. Per-page registries below list the widgets each page
   may show; dashboard ids match the server catalogue
   (admin-dashboard context). Widgets are for admin pages only —
   the front page (index.html) and the tenant site (tenant.html)
   have no widgets. The registry key here is the page's widget
   zone; on the server this page is the `admin-dashboard` zone.
   ========================================================================== */

(function () {
  "use strict";

  var REGISTRY = {
    dashboard: [
      { id: "stat-cards", title: "Key figures", pinned: true },
      { id: "market-strip", title: "Market strip" },
      { id: "announcements-ticker", title: "Announcements" },
      { id: "module-grid", title: "Modules" },
      { id: "recent-activity", title: "Recent activity" },
      { id: "system-status", title: "System status" },
    ],
  };

  function $(selector, root) {
    return (root || document).querySelector(selector);
  }

  function $all(selector, root) {
    return Array.prototype.slice.call((root || document).querySelectorAll(selector));
  }

  var toastTimer = null;

  function demoToast(message) {
    var toast = document.getElementById("demo-toast");
    if (!toast) return;
    toast.textContent = message;
    toast.classList.add("show");
    if (toastTimer) clearTimeout(toastTimer);
    toastTimer = setTimeout(function () {
      toast.classList.remove("show");
    }, 2600);
  }

  function storageKey(page) {
    return "unified-demo-widgets:" + page;
  }

  function loadLayout(page) {
    try {
      var raw = localStorage.getItem(storageKey(page));
      if (!raw) return null;
      var parsed = JSON.parse(raw);
      if (!parsed || !Array.isArray(parsed.order)) return null;
      var strings = function (value) {
        return Array.isArray(value)
          ? value.filter(function (id) { return typeof id === "string"; })
          : [];
      };
      return {
        order: strings(parsed.order),
        pinned: strings(parsed.pinned),
        removed: strings(parsed.removed),
      };
    } catch (error) {
      return null;
    }
  }

  function saveLayout(page, state) {
    try {
      localStorage.setItem(storageKey(page), JSON.stringify({
        order: state.order,
        pinned: state.pinned,
        removed: state.removed,
      }));
    } catch (error) {
      // Private mode or full storage: the layout just does not persist.
    }
  }

  function widgetState(page) {
    var registry = REGISTRY[page] || [];
    var known = registry.map(function (entry) { return entry.id; });
    var byId = {};
    $all("[data-widget]").forEach(function (el) {
      byId[el.getAttribute("data-widget")] = el;
    });
    var order = known.slice();
    var pinned = registry
      .filter(function (entry) { return entry.pinned; })
      .map(function (entry) { return entry.id; });
    var removed = [];
    var saved = loadLayout(page);
    if (saved) {
      order = saved.order.filter(function (id) { return known.indexOf(id) !== -1; });
      known.forEach(function (id) {
        if (order.indexOf(id) === -1) order.push(id);
      });
      pinned = saved.pinned.filter(function (id) { return known.indexOf(id) !== -1; });
      removed = saved.removed.filter(function (id) { return known.indexOf(id) !== -1; });
    }
    return { page: page, registry: registry, byId: byId, order: order, pinned: pinned, removed: removed };
  }

  /** Pinned first (stable), then the rest, in state.order. */
  function sortedOrder(state) {
    var pinned = state.order.filter(function (id) { return state.pinned.indexOf(id) !== -1; });
    var rest = state.order.filter(function (id) { return state.pinned.indexOf(id) === -1; });
    return pinned.concat(rest);
  }

  function collapseEmptyRows() {
    $all(".widget-row").forEach(function (row) {
      var visible = $all("[data-widget]", row).filter(function (el) { return !el.hidden; });
      row.hidden = visible.length === 0;
    });
  }

  function applyLayout(state) {
    var ordered = sortedOrder(state);
    state.order = ordered;
    var prev = null;
    var firstVisible = $all("[data-widget]").filter(function (el) { return !el.hidden; })[0];
    ordered.forEach(function (id) {
      var el = state.byId[id];
      if (!el) return;
      var isRemoved = state.removed.indexOf(id) !== -1;
      el.hidden = isRemoved;
      el.classList.toggle("widget-pinned", state.pinned.indexOf(id) !== -1);
      var pinButton = $(".widget-pin", el);
      if (pinButton) {
        var isPinned = state.pinned.indexOf(id) !== -1;
        pinButton.textContent = isPinned ? "Pinned" : "Pin";
        pinButton.setAttribute("aria-pressed", isPinned ? "true" : "false");
      }
      if (isRemoved) return;
      if (prev === null) {
        if (firstVisible && el !== firstVisible) {
          firstVisible.parentNode.insertBefore(el, firstVisible);
        }
      } else {
        prev.parentNode.insertBefore(el, prev.nextSibling);
      }
      prev = el;
    });
    collapseEmptyRows();
  }

  function refreshPicker(state) {
    var select = document.getElementById("widget-add-select");
    var addButton = document.getElementById("widget-add");
    if (!select || !addButton) return;
    var removed = state.removed.filter(function (id) { return state.byId[id]; });
    select.innerHTML = "";
    removed.forEach(function (id) {
      var entry = null;
      state.registry.forEach(function (item) {
        if (item.id === id) entry = item;
      });
      var option = document.createElement("option");
      option.value = id;
      option.textContent = entry ? entry.title : id;
      select.appendChild(option);
    });
    var empty = removed.length === 0;
    select.disabled = empty;
    addButton.disabled = empty;
    if (empty) {
      var option = document.createElement("option");
      option.textContent = "No removed widgets";
      select.appendChild(option);
    }
  }

  function buildPicker(state) {
    var note = $(".demo-note");
    if (!note) return;
    var toolbar = document.createElement("div");
    toolbar.className = "widget-toolbar";
    toolbar.innerHTML =
      '<label for="widget-add-select">Add widget</label>' +
      '<select id="widget-add-select"></select>' +
      '<button type="button" class="button button-small" id="widget-add">Add</button>' +
      '<button type="button" class="button button-small button-secondary" id="widget-reset">Reset layout</button>' +
      '<span class="small muted">Drag a widget by its handle to reorder it. Layouts save in this browser only.</span>';
    note.parentNode.insertBefore(toolbar, note.nextSibling);
    $("#widget-add").addEventListener("click", function () {
      var select = document.getElementById("widget-add-select");
      var id = select.value;
      var el = state.byId[id];
      if (!el) return;
      state.removed = state.removed.filter(function (item) { return item !== id; });
      el.hidden = false;
      // Added widgets go last (after the last visible widget).
      var visible = $all("[data-widget]").filter(function (item) { return !item.hidden && item !== el; });
      var last = visible[visible.length - 1];
      if (last) last.parentNode.insertBefore(el, last.nextSibling);
      state.order = $all("[data-widget]").map(function (item) { return item.getAttribute("data-widget"); });
      saveLayout(state.page, state);
      applyLayout(state);
      refreshPicker(state);
      demoToast("Widget added (demo only).");
    });
    $("#widget-reset").addEventListener("click", function () {
      try { localStorage.removeItem(storageKey(state.page)); } catch (error) { /* ignore */ }
      window.location.reload();
    });
    refreshPicker(state);
  }

  function injectBars(state) {
    state.registry.forEach(function (entry) {
      var el = state.byId[entry.id];
      if (!el || $(".widget-bar", el)) return;
      var title = el.getAttribute("data-widget-title") || entry.title;
      var bar = document.createElement("div");
      bar.className = "widget-bar";
      bar.innerHTML =
        '<span class="widget-handle" title="Drag to reorder">Drag</span>' +
        '<span class="widget-title"></span>' +
        '<button type="button" class="widget-pin" aria-pressed="false">Pin</button>' +
        '<button type="button" class="widget-remove">Remove</button>';
      $(".widget-title", bar).textContent = title;
      el.insertBefore(bar, el.firstChild);
    });
  }

  function currentOrderFromDom() {
    return $all("[data-widget]").map(function (el) { return el.getAttribute("data-widget"); });
  }

  function initWidgets() {
    var page = document.body.getAttribute("data-page");
    if (!page || !REGISTRY[page]) return;
    var state = widgetState(page);
    injectBars(state);
    buildPicker(state);
    applyLayout(state);

    document.addEventListener("click", function (event) {
      var button = event.target.closest(".widget-pin, .widget-remove");
      if (!button) return;
      var widget = button.closest("[data-widget]");
      if (!widget) return;
      var id = widget.getAttribute("data-widget");
      if (button.classList.contains("widget-pin")) {
        var isPinned = state.pinned.indexOf(id) !== -1;
        state.pinned = isPinned
          ? state.pinned.filter(function (item) { return item !== id; })
          : state.pinned.concat([id]);
        saveLayout(state.page, state);
        applyLayout(state);
        demoToast(isPinned ? "Widget unpinned (demo only)." : "Widget pinned — pinned widgets stay at the top (demo only).");
      } else {
        state.removed.push(id);
        saveLayout(state.page, state);
        applyLayout(state);
        refreshPicker(state);
        demoToast("Widget removed — add it back from the picker (demo only).");
      }
    });

    // Drag-to-reorder, Pointer Events only. The widget leaves the
    // flow (position: fixed, pointer-events: none) while a
    // placeholder holds its slot; the placeholder moves with the
    // pointer and the widget drops where it sits.
    var drag = null;

    document.addEventListener("pointerdown", function (event) {
      var handle = event.target.closest ? event.target.closest(".widget-handle") : null;
      if (!handle) return;
      if (event.button !== undefined && event.button !== 0) return;
      var widget = handle.closest("[data-widget]");
      if (!widget || widget.hidden) return;
      event.preventDefault();
      var rect = widget.getBoundingClientRect();
      var placeholder = document.createElement("div");
      placeholder.className = "widget-placeholder";
      placeholder.style.height = rect.height + "px";
      widget.parentNode.insertBefore(placeholder, widget);
      widget.classList.add("widget-dragging");
      widget.style.width = rect.width + "px";
      widget.style.left = rect.left + "px";
      widget.style.top = rect.top + "px";
      drag = {
        widget: widget,
        placeholder: placeholder,
        offsetX: event.clientX - rect.left,
        offsetY: event.clientY - rect.top,
      };
      if (handle.setPointerCapture && event.pointerId !== undefined) {
        try { handle.setPointerCapture(event.pointerId); } catch (error) { /* older browsers */ }
      }
    });

    document.addEventListener("pointermove", function (event) {
      if (!drag) return;
      drag.widget.style.left = (event.clientX - drag.offsetX) + "px";
      drag.widget.style.top = (event.clientY - drag.offsetY) + "px";
      var under = document.elementFromPoint(event.clientX, event.clientY);
      var target = under && under.closest ? under.closest("[data-widget]") : null;
      if (!target || target === drag.widget) return;
      var targetRect = target.getBoundingClientRect();
      var before = event.clientY < targetRect.top + targetRect.height / 2;
      if (before) {
        target.parentNode.insertBefore(drag.placeholder, target);
      } else {
        target.parentNode.insertBefore(drag.placeholder, target.nextSibling);
      }
    });

    document.addEventListener("pointerup", function () {
      if (!drag) return;
      var widget = drag.widget;
      var placeholder = drag.placeholder;
      placeholder.parentNode.insertBefore(widget, placeholder);
      placeholder.remove();
      widget.classList.remove("widget-dragging");
      widget.style.width = "";
      widget.style.left = "";
      widget.style.top = "";
      drag = null;
      state.order = currentOrderFromDom();
      saveLayout(state.page, state);
      applyLayout(state);
      demoToast("Layout saved in this browser (demo only).");
    });

    document.addEventListener("pointercancel", function () {
      if (!drag) return;
      var widget = drag.widget;
      var placeholder = drag.placeholder;
      placeholder.parentNode.insertBefore(widget, placeholder);
      placeholder.remove();
      widget.classList.remove("widget-dragging");
      widget.style.width = "";
      widget.style.left = "";
      widget.style.top = "";
      drag = null;
      applyLayout(state);
    });
  }

  document.addEventListener("DOMContentLoaded", initWidgets);
})();

/* ==========================================================================
   Site Builder — task 353

   Demo of the GrapeJS-based site builder (the real editor is bundled
   as assets/site-builder/dist/editor.js — no CDN). The canvas shows
   the home page's blocks; the palette adds blocks (click to append,
   drag to insert), the traits panel edits the selected block's
   fields, text blocks edit in place, and Save/Publish persist to
   localStorage, browser only. Blocks reorder by Pointer Events.
   ========================================================================== */

(function () {
  "use strict";

  var STORAGE_KEY = "unified-demo-site-builder:home";

  var FIELDS = {
    hero: [
      { name: "heading", label: "Heading", kind: "text" },
      { name: "subheading", label: "Subheading", kind: "text" },
      { name: "cta_text", label: "Button text", kind: "text" },
      { name: "cta_url", label: "Button URL", kind: "url" },
    ],
    text: [{ name: "body", label: "Body", kind: "textarea" }],
    image: [
      { name: "url", label: "Image URL", kind: "url" },
      { name: "alt", label: "Alt text", kind: "text" },
    ],
    gallery: [{ name: "images", label: "Image URLs (one per line)", kind: "textarea" }],
    form: [
      { name: "submit_label", label: "Submit label", kind: "text" },
      { name: "action", label: "Form action URL", kind: "url" },
    ],
    custom: [{ name: "html", label: "HTML", kind: "textarea" }],
    pricing_table: [{ name: "pricing_table_id", label: "Pricing table ID", kind: "text" }],
    marketplace_storefront: [{ name: "vendor_slug", label: "Vendor slug", kind: "text" }],
  };

  var BLOCK_LABELS = {
    hero: "Hero",
    text: "Text",
    image: "Image",
    gallery: "Gallery",
    form: "Form",
    custom: "Custom",
    pricing_table: "Pricing table",
    marketplace_storefront: "Storefront",
  };

  function $(selector, root) {
    return (root || document).querySelector(selector);
  }

  function $all(selector, root) {
    return Array.prototype.slice.call((root || document).querySelectorAll(selector));
  }

  function escapeHtml(value) {
    return String(value)
      .replace(/&/g, "&amp;")
      .replace(/</g, "&lt;")
      .replace(/>/g, "&gt;")
      .replace(/"/g, "&quot;");
  }

  var toastTimer = null;

  function demoToast(message) {
    var toast = document.getElementById("demo-toast");
    if (!toast) return;
    toast.textContent = message;
    toast.classList.add("show");
    if (toastTimer) clearTimeout(toastTimer);
    toastTimer = setTimeout(function () {
      toast.classList.remove("show");
    }, 2600);
  }

  var nextId = 1;

  function newId() {
    nextId += 1;
    return "blk-" + Date.now().toString(36) + "-" + nextId;
  }

  function defaultContent(type) {
    switch (type) {
      case "hero":
        return { heading: "New section", subheading: "", cta_text: "Learn more", cta_url: "modules.html" };
      case "text":
        return { body: "Write something…" };
      case "image":
        return { url: "", alt: "" };
      case "gallery":
        return { images: "" };
      case "form":
        return { submit_label: "Submit", action: "" };
      case "custom":
        return { html: "<p>Custom HTML</p>" };
      case "pricing_table":
        return { pricing_table_id: "" };
      case "marketplace_storefront":
        return { vendor_slug: "" };
      default:
        return {};
    }
  }

  function fieldValue(block, name) {
    var value = block.content[name];
    return value === undefined || value === null ? "" : String(value);
  }

  /** Fill a block element's body from its model entry. Text nodes are
   * set with textContent, never innerHTML — user-typed content
   * (including the custom block's HTML) is shown as text, not run. */
  function renderBlockBody(el, block) {
    var body = $(".sb-block-body", el);
    if (!body) return;
    body.innerHTML = "";
    var type = block.type;
    if (type === "hero") {
      var heading = document.createElement("h2");
      heading.setAttribute("data-sb-field", "heading");
      heading.setAttribute("contenteditable", "true");
      heading.setAttribute("spellcheck", "false");
      heading.textContent = fieldValue(block, "heading");
      body.appendChild(heading);
      var sub = document.createElement("p");
      sub.setAttribute("data-sb-field", "subheading");
      sub.setAttribute("contenteditable", "true");
      sub.setAttribute("spellcheck", "false");
      sub.textContent = fieldValue(block, "subheading");
      body.appendChild(sub);
      var ctaWrap = document.createElement("p");
      var cta = document.createElement("a");
      cta.className = "button";
      cta.setAttribute("data-sb-field", "cta_text");
      cta.setAttribute("contenteditable", "true");
      cta.setAttribute("spellcheck", "false");
      cta.setAttribute("href", fieldValue(block, "cta_url") || "#");
      cta.textContent = fieldValue(block, "cta_text");
      ctaWrap.appendChild(cta);
      body.appendChild(ctaWrap);
    } else if (type === "text") {
      var text = document.createElement("p");
      text.setAttribute("data-sb-field", "body");
      text.setAttribute("contenteditable", "true");
      text.setAttribute("spellcheck", "false");
      text.textContent = fieldValue(block, "body");
      body.appendChild(text);
    } else if (type === "image") {
      var url = fieldValue(block, "url");
      if (url) {
        var img = document.createElement("img");
        img.setAttribute("src", url);
        img.setAttribute("alt", fieldValue(block, "alt"));
        img.style.maxWidth = "100%";
        body.appendChild(img);
      } else {
        var noImage = document.createElement("p");
        noImage.className = "small muted";
        noImage.textContent = "No image yet — set a URL in Block settings.";
        body.appendChild(noImage);
      }
    } else if (type === "gallery") {
      var lines = fieldValue(block, "images").split("\n").map(function (line) { return line.trim(); }).filter(Boolean);
      if (lines.length === 0) {
        var noGallery = document.createElement("p");
        noGallery.className = "small muted";
        noGallery.textContent = "No images yet — one URL per line in Block settings.";
        body.appendChild(noGallery);
      } else {
        lines.forEach(function (line, index) {
          var item = document.createElement("p");
          item.textContent = "Image " + (index + 1) + ": " + line;
          body.appendChild(item);
        });
      }
    } else if (type === "form") {
      var formNote = document.createElement("p");
      formNote.textContent = "Form — submit button: " + (fieldValue(block, "submit_label") || "Submit");
      body.appendChild(formNote);
      var action = fieldValue(block, "action");
      if (action) {
        var actionNote = document.createElement("p");
        actionNote.className = "small muted";
        actionNote.textContent = "Posts to: " + action;
        body.appendChild(actionNote);
      }
    } else if (type === "custom") {
      var code = document.createElement("code");
      code.textContent = fieldValue(block, "html");
      body.appendChild(code);
    } else if (type === "pricing_table") {
      var pricingNote = document.createElement("p");
      var pricingId = fieldValue(block, "pricing_table_id");
      pricingNote.textContent = pricingId ? "Pricing table: " + pricingId : "No pricing table selected.";
      body.appendChild(pricingNote);
    } else if (type === "marketplace_storefront") {
      var storeNote = document.createElement("p");
      var slug = fieldValue(block, "vendor_slug");
      storeNote.textContent = slug ? "Storefront: " + slug : "No vendor selected.";
      body.appendChild(storeNote);
    } else {
      var unknown = document.createElement("p");
      unknown.className = "small muted";
      unknown.textContent = "Unknown block type: " + type;
      body.appendChild(unknown);
    }
  }

  function createBlockElement(block) {
    var el = document.createElement("article");
    el.className = "sb-block";
    el.setAttribute("data-block-type", block.type);
    el.setAttribute("data-block-id", block.id);
    var bar = document.createElement("div");
    bar.className = "sb-block-bar";
    bar.innerHTML =
      '<span class="sb-block-handle" title="Drag to reorder">Drag</span>' +
      '<span class="sb-block-type">' + escapeHtml(BLOCK_LABELS[block.type] || block.type) + "</span>" +
      '<button type="button" class="sb-block-remove" data-sb-remove-block>Remove</button>';
    el.appendChild(bar);
    var body = document.createElement("div");
    body.className = "sb-block-body";
    el.appendChild(body);
    renderBlockBody(el, block);
    return el;
  }

  function parseBlock(el) {
    var type = el.getAttribute("data-block-type") || "text";
    var content = {};
    (FIELDS[type] || []).forEach(function (field) {
      if (field.name === "cta_url") {
        // The CTA link carries the text field; its URL is the href.
        var cta = el.querySelector('[data-sb-field="cta_text"]');
        content[field.name] = cta ? cta.getAttribute("href") || "" : "";
        return;
      }
      var node = el.querySelector('[data-sb-field="' + field.name + '"]');
      if (!node) return;
      content[field.name] = node.textContent;
    });
    return {
      id: el.getAttribute("data-block-id") || newId(),
      type: type,
      content: content,
    };
  }

  function loadSaved() {
    try {
      var raw = localStorage.getItem(STORAGE_KEY);
      if (!raw) return null;
      var parsed = JSON.parse(raw);
      if (!parsed || !Array.isArray(parsed.blocks)) return null;
      var blocks = parsed.blocks.filter(function (block) {
        return block && typeof block.id === "string" && typeof block.type === "string" &&
          block.content && typeof block.content === "object";
      });
      if (blocks.length === 0) return null;
      return { blocks: blocks, status: parsed.status === "draft" ? "draft" : "published" };
    } catch (error) {
      return null;
    }
  }

  function initSiteBuilder() {
    if (document.body.getAttribute("data-page") !== "site-builder") return;
    var canvas = document.getElementById("sb-canvas");
    var traits = document.getElementById("sb-traits");
    var statusBadge = document.getElementById("sb-status");
    if (!canvas || !traits) return;

    var saved = loadSaved();
    var blocks = saved
      ? saved.blocks
      : $all(".sb-block", canvas).map(parseBlock);
    var status = saved ? saved.status : "published";
    var selectedId = null;

    function blockById(id) {
      var found = null;
      blocks.forEach(function (block) {
        if (block.id === id) found = block;
      });
      return found;
    }

    function elementById(id) {
      return $('.sb-block[data-block-id="' + id + '"]', canvas);
    }

    function renderCanvas() {
      canvas.innerHTML = "";
      blocks.forEach(function (block) {
        canvas.appendChild(createBlockElement(block));
      });
    }

    function renderStatus() {
      if (!statusBadge) return;
      statusBadge.textContent = status === "published" ? "Published" : "Draft";
      statusBadge.classList.toggle("badge-success", status === "published");
      statusBadge.classList.toggle("badge-warning", status !== "published");
    }

    function renderTraits() {
      traits.innerHTML = "";
      var block = selectedId ? blockById(selectedId) : null;
      if (!block) {
        var hint = document.createElement("p");
        hint.className = "small muted";
        hint.textContent = "Select a block on the canvas to edit its fields here.";
        traits.appendChild(hint);
        return;
      }
      var title = document.createElement("p");
      title.innerHTML = "<strong>" + escapeHtml(BLOCK_LABELS[block.type] || block.type) + "</strong>";
      traits.appendChild(title);
      var fields = FIELDS[block.type] || [];
      if (fields.length === 0) {
        var info = document.createElement("p");
        info.className = "small muted";
        info.textContent = "Live data — this widget has no fields to edit.";
        traits.appendChild(info);
        return;
      }
      fields.forEach(function (field) {
        var wrap = document.createElement("div");
        wrap.className = "field";
        var label = document.createElement("label");
        label.textContent = field.label;
        label.setAttribute("for", "sb-trait-" + field.name);
        wrap.appendChild(label);
        var input;
        if (field.kind === "textarea") {
          input = document.createElement("textarea");
          input.rows = 4;
        } else {
          input = document.createElement("input");
          input.type = field.kind === "number" ? "number" : field.kind === "url" ? "url" : "text";
        }
        input.id = "sb-trait-" + field.name;
        input.value = fieldValue(block, field.name);
        input.addEventListener("input", function () {
          block.content[field.name] = input.value;
          var el = elementById(block.id);
          if (el) renderBlockBody(el, block);
        });
        wrap.appendChild(input);
        traits.appendChild(wrap);
      });
    }

    function selectBlock(id) {
      selectedId = id;
      $all(".sb-block", canvas).forEach(function (el) {
        el.classList.toggle("sb-selected", el.getAttribute("data-block-id") === id);
      });
      renderTraits();
    }

    function save() {
      try {
        localStorage.setItem(STORAGE_KEY, JSON.stringify({ blocks: blocks, status: status }));
      } catch (error) {
        demoToast("Could not save — browser storage is unavailable.");
        return;
      }
      demoToast("Blocks saved (demo only).");
    }

    function syncOrderFromDom() {
      var order = $all(".sb-block", canvas).map(function (el) { return el.getAttribute("data-block-id"); });
      blocks.sort(function (a, b) { return order.indexOf(a.id) - order.indexOf(b.id); });
    }

    // Seed from the static HTML, then render from the model so the
    // canvas and the traits panel always agree.
    renderCanvas();
    renderStatus();
    renderTraits();

    canvas.addEventListener("click", function (event) {
      var removeButton = event.target.closest ? event.target.closest("[data-sb-remove-block]") : null;
      var blockEl = event.target.closest ? event.target.closest(".sb-block") : null;
      if (!blockEl) return;
      var id = blockEl.getAttribute("data-block-id");
      if (removeButton) {
        blocks = blocks.filter(function (block) { return block.id !== id; });
        if (selectedId === id) selectedId = null;
        renderCanvas();
        renderTraits();
        demoToast("Block removed (demo only).");
        return;
      }
      selectBlock(id);
    });

    // In-place text editing: the canvas writes back to the model.
    canvas.addEventListener("input", function (event) {
      var field = event.target.closest ? event.target.closest("[data-sb-field]") : null;
      if (!field) return;
      var blockEl = field.closest(".sb-block");
      if (!blockEl) return;
      var block = blockById(blockEl.getAttribute("data-block-id"));
      if (!block) return;
      block.content[field.getAttribute("data-sb-field")] = field.textContent;
      if (selectedId === block.id) renderTraits();
    });

    // Palette: a tap (pointerdown + pointerup without moving) appends
    // at the end — handled in pointerup, not in a click handler,
    // because the drag's preventDefault() may suppress the click.
    // Drag-to-reorder (canvas handles) and drag-to-insert (palette
    // items). Pointer Events only; a dragged palette block dropped
    // outside the canvas is discarded.
    var drag = null;

    document.addEventListener("pointerdown", function (event) {
      var handle = event.target.closest ? event.target.closest(".sb-block-handle") : null;
      var paletteItem = event.target.closest ? event.target.closest("[data-sb-palette-type]") : null;
      if (!handle && !paletteItem) return;
      if (event.button !== undefined && event.button !== 0) return;
      event.preventDefault();
      var canvasRect = canvas.getBoundingClientRect();
      var placeholder = document.createElement("div");
      placeholder.className = "widget-placeholder";
      if (handle) {
        var widget = handle.closest(".sb-block");
        if (!widget) return;
        var rect = widget.getBoundingClientRect();
        placeholder.style.height = rect.height + "px";
        widget.parentNode.insertBefore(placeholder, widget);
        widget.classList.add("sb-block-dragging");
        widget.style.width = rect.width + "px";
        widget.style.left = rect.left + "px";
        widget.style.top = rect.top + "px";
        drag = {
          el: widget,
          placeholder: placeholder,
          offsetX: event.clientX - rect.left,
          offsetY: event.clientY - rect.top,
          startX: event.clientX,
          startY: event.clientY,
          isNew: false,
          canvasRect: canvasRect,
        };
      } else {
        var type = paletteItem.getAttribute("data-sb-palette-type");
        var block = { id: newId(), type: type, content: defaultContent(type) };
        var el = createBlockElement(block);
        var paletteRect = paletteItem.getBoundingClientRect();
        placeholder.style.height = "120px";
        canvas.appendChild(placeholder);
        el.classList.add("sb-block-dragging");
        el.style.width = Math.min(paletteRect.width, canvasRect.width) + "px";
        el.style.left = (event.clientX - paletteRect.width / 2) + "px";
        el.style.top = (event.clientY - 20) + "px";
        document.body.appendChild(el);
        drag = {
          el: el,
          placeholder: placeholder,
          offsetX: paletteRect.width / 2,
          offsetY: 20,
          startX: event.clientX,
          startY: event.clientY,
          isNew: true,
          block: block,
          canvasRect: canvasRect,
        };
      }
      var target = handle || paletteItem;
      if (target.setPointerCapture && event.pointerId !== undefined) {
        try { target.setPointerCapture(event.pointerId); } catch (error) { /* older browsers */ }
      }
    });

    document.addEventListener("pointermove", function (event) {
      if (!drag) return;
      drag.el.style.left = (event.clientX - drag.offsetX) + "px";
      drag.el.style.top = (event.clientY - drag.offsetY) + "px";
      var under = document.elementFromPoint(event.clientX, event.clientY);
      var target = under && under.closest ? under.closest(".sb-block") : null;
      var overCanvas = under && under.closest ? !!under.closest("#sb-canvas") : false;
      if (target && target !== drag.el) {
        var targetRect = target.getBoundingClientRect();
        var before = event.clientY < targetRect.top + targetRect.height / 2;
        if (before) {
          target.parentNode.insertBefore(drag.placeholder, target);
        } else {
          target.parentNode.insertBefore(drag.placeholder, target.nextSibling);
        }
      } else if (overCanvas && drag.placeholder.parentNode !== canvas) {
        canvas.appendChild(drag.placeholder);
      }
    });

    function endDrag(commit) {
      if (!drag) return;
      var el = drag.el;
      var placeholder = drag.placeholder;
      if (commit && placeholder.parentNode === canvas) {
        placeholder.parentNode.insertBefore(el, placeholder);
        if (drag.isNew) {
          blocks.splice(
            $all(".sb-block", canvas).indexOf(el),
            0,
            drag.block,
          );
          selectBlock(drag.block.id);
        }
        syncOrderFromDom();
      } else if (!drag.isNew) {
        placeholder.parentNode.insertBefore(el, placeholder);
      } else {
        el.remove();
        demoToast("Dropped outside the canvas — block discarded.");
      }
      placeholder.remove();
      el.classList.remove("sb-block-dragging");
      el.style.width = "";
      el.style.left = "";
      el.style.top = "";
      drag = null;
    }

    document.addEventListener("pointerup", function (event) {
      if (!drag) return;
      var moved = Math.hypot(event.clientX - drag.startX, event.clientY - drag.startY) > 5;
      var under = document.elementFromPoint(event.clientX, event.clientY);
      var overCanvas = !!(under && under.closest && (under.closest("#sb-canvas") || under.closest(".sb-block")));
      // A palette tap (no movement) appends at the end, where the
      // placeholder already sits. A palette drag only commits when
      // it ends over the canvas. A canvas drag always lands where
      // its placeholder sits.
      endDrag(drag.isNew ? (moved ? overCanvas : true) : true);
    });

    document.addEventListener("pointercancel", function () {
      endDrag(false);
    });

    var saveButton = document.getElementById("sb-save");
    if (saveButton) saveButton.addEventListener("click", save);

    var publishButton = document.getElementById("sb-publish");
    if (publishButton) {
      publishButton.addEventListener("click", function () {
        status = "published";
        renderStatus();
        save();
        demoToast("Page published (demo only).");
      });
    }

    var unpublishButton = document.getElementById("sb-unpublish");
    if (unpublishButton) {
      unpublishButton.addEventListener("click", function () {
        status = "draft";
        renderStatus();
        save();
        demoToast("Page unpublished — now a draft (demo only).");
      });
    }

    var previewButton = document.getElementById("sb-preview");
    if (previewButton) {
      previewButton.addEventListener("click", function () {
        window.open("index.html", "_blank");
      });
    }
  }

  document.addEventListener("DOMContentLoaded", initSiteBuilder);
})();
