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
  var DEMO_MAIL_PROVIDERS = [{"id": "sendmail", "name": "Local Sendmail", "logo": "email-sendmail", "note": "Delivers through the local sendmail binary \u2014 no credentials needed. The default builtin, like PHP mail().", "fields": [{"key": "sender_email", "label": "Default sender email", "type": "email", "placeholder": "noreply@example.com", "help": "From address used when a send does not supply its own; defaults to unified@localhost."}, {"key": "sender_name", "label": "Default sender name", "type": "text", "placeholder": "Unified"}, {"key": "test_mode", "label": "Test mode (sandbox)", "type": "checkbox", "help": "When on, mail is recorded as a simulated delivery and never sent."}]}, {"id": "smtp", "name": "SMTP (Custom server)", "logo": "email-smtp", "note": "Direct SMTP delivery to any server: host, port, TLS mode and credentials live on the connection.", "fields": [{"key": "host", "label": "SMTP host", "type": "text", "required": true, "placeholder": "smtp.example.com"}, {"key": "port", "label": "SMTP port", "type": "number", "required": true, "placeholder": "587", "help": "587 for STARTTLS, 465 for implicit TLS."}, {"key": "tls_mode", "label": "TLS mode", "type": "select", "required": true, "options": [["starttls", "STARTTLS (port 587)"], ["implicit", "Implicit TLS (port 465)"], ["none", "None (plaintext, local servers only)"]]}, {"key": "username", "label": "SMTP username", "type": "text", "placeholder": "user@example.com", "help": "Usually the sender address. Leave empty for servers without authentication."}, {"key": "password", "label": "SMTP password", "type": "password", "help": "Stored encrypted; never shown again."}, {"key": "sender_email", "label": "Default sender email", "type": "email", "required": true, "placeholder": "news@example.com"}, {"key": "sender_name", "label": "Default sender name", "type": "text", "placeholder": "Unified"}, {"key": "test_mode", "label": "Test mode (sandbox)", "type": "checkbox", "help": "When on, mail is recorded as a simulated delivery and never sent."}]}, {"id": "gmail", "name": "Gmail", "logo": "email-gmail", "note": "Sends through smtp.gmail.com:587 with OAuth2 (XOAUTH2) \u2014 no app passwords.", "oauth": "Google", "fields": [{"key": "client_id", "label": "OAuth client ID", "type": "text", "required": true}, {"key": "client_secret", "label": "OAuth client secret", "type": "password", "required": true, "help": "Stored encrypted; never shown again."}, {"key": "sender_email", "label": "Sender email", "type": "email", "required": true, "placeholder": "you@gmail.com", "help": "The linked account address \u2014 the XOAUTH2 identity."}, {"key": "sender_name", "label": "Default sender name", "type": "text", "placeholder": "Unified"}, {"key": "test_mode", "label": "Test mode (sandbox)", "type": "checkbox", "help": "When on, mail is recorded as a simulated delivery and never sent."}]}, {"id": "yahoo", "name": "Yahoo Mail", "logo": "email-yahoo", "note": "Sends through smtp.mail.yahoo.com:587 with OAuth2 (XOAUTH2).", "oauth": "Yahoo", "fields": [{"key": "client_id", "label": "OAuth client ID", "type": "text", "required": true}, {"key": "client_secret", "label": "OAuth client secret", "type": "password", "required": true, "help": "Stored encrypted; never shown again."}, {"key": "sender_email", "label": "Sender email", "type": "email", "required": true, "placeholder": "you@yahoo.com", "help": "The linked account address \u2014 the XOAUTH2 identity."}, {"key": "sender_name", "label": "Default sender name", "type": "text", "placeholder": "Unified"}, {"key": "test_mode", "label": "Test mode (sandbox)", "type": "checkbox", "help": "When on, mail is recorded as a simulated delivery and never sent."}]}, {"id": "outlook", "name": "Outlook / Microsoft 365", "logo": "email-outlook", "note": "Sends through smtp.office365.com:587 with OAuth2 (XOAUTH2).", "oauth": "Microsoft", "fields": [{"key": "client_id", "label": "OAuth client ID", "type": "text", "required": true}, {"key": "client_secret", "label": "OAuth client secret", "type": "password", "required": true, "help": "Stored encrypted; never shown again."}, {"key": "sender_email", "label": "Sender email", "type": "email", "required": true, "placeholder": "you@outlook.com", "help": "The linked account address \u2014 the XOAUTH2 identity."}, {"key": "sender_name", "label": "Default sender name", "type": "text", "placeholder": "Unified"}, {"key": "test_mode", "label": "Test mode (sandbox)", "type": "checkbox", "help": "When on, mail is recorded as a simulated delivery and never sent."}]}, {"id": "sendgrid", "name": "SendGrid", "logo": "email-sendgrid", "note": "Deliverability platform with a sandbox for test mail.", "fields": [{"key": "api_key", "label": "API key", "type": "password", "required": true}, {"key": "sender_email", "label": "Default sender email", "type": "email", "placeholder": "news@example.com", "help": "From address used when a send does not supply its own."}, {"key": "sender_name", "label": "Default sender name", "type": "text", "placeholder": "Unified"}, {"key": "webhook_secret", "label": "Webhook secret / signing key", "type": "password", "help": "Secret used to verify incoming webhooks from this provider."}, {"key": "test_mode", "label": "Test mode (sandbox)", "type": "checkbox", "help": "When on, mail is recorded as a simulated delivery and never sent."}]}, {"id": "mailgun", "name": "Mailgun", "logo": "email-mailgun", "note": "Transactional mail with a verified sending domain.", "fields": [{"key": "api_key", "label": "API key", "type": "password", "required": true}, {"key": "domain", "label": "Mailgun domain", "type": "text", "required": true, "placeholder": "mg.example.com"}, {"key": "region", "label": "Region", "type": "select", "options": [["us", "US (api.mailgun.net)"], ["eu", "EU (api.eu.mailgun.net)"]]}, {"key": "sender_email", "label": "Default sender email", "type": "email", "placeholder": "news@example.com", "help": "From address used when a send does not supply its own."}, {"key": "sender_name", "label": "Default sender name", "type": "text", "placeholder": "Unified"}, {"key": "webhook_secret", "label": "Webhook secret / signing key", "type": "password", "help": "Secret used to verify incoming webhooks from this provider."}, {"key": "test_mode", "label": "Test mode (sandbox)", "type": "checkbox", "help": "When on, mail is recorded as a simulated delivery and never sent."}]}, {"id": "postmark", "name": "Postmark", "logo": "email-postmark", "note": "Transactional mail with sender signatures.", "fields": [{"key": "api_key", "label": "API key", "type": "password", "required": true}, {"key": "sender_email", "label": "Default sender email", "type": "email", "placeholder": "news@example.com", "help": "From address used when a send does not supply its own."}, {"key": "sender_name", "label": "Default sender name", "type": "text", "placeholder": "Unified"}, {"key": "webhook_secret", "label": "Webhook secret / signing key", "type": "password", "help": "Secret used to verify incoming webhooks from this provider."}, {"key": "test_mode", "label": "Test mode (sandbox)", "type": "checkbox", "help": "When on, mail is recorded as a simulated delivery and never sent."}]}, {"id": "ses", "name": "Amazon SES", "logo": "email-ses", "note": "Amazon Simple Email Service (SigV4).", "fields": [{"key": "api_key", "label": "API key", "type": "password", "required": true}, {"key": "access_key_id", "label": "Access key ID", "type": "text", "required": true}, {"key": "secret_access_key", "label": "Secret access key", "type": "password", "required": true}, {"key": "region", "label": "Region", "type": "text", "required": true, "placeholder": "us-east-1"}, {"key": "sender_email", "label": "Default sender email", "type": "email", "placeholder": "news@example.com", "help": "From address used when a send does not supply its own."}, {"key": "sender_name", "label": "Default sender name", "type": "text", "placeholder": "Unified"}, {"key": "webhook_secret", "label": "Webhook secret / signing key", "type": "password", "help": "Secret used to verify incoming webhooks from this provider."}, {"key": "test_mode", "label": "Test mode (sandbox)", "type": "checkbox", "help": "When on, mail is recorded as a simulated delivery and never sent."}]}, {"id": "resend", "name": "Resend", "logo": "email-resend", "note": "Transactional mail for developers.", "fields": [{"key": "api_key", "label": "API key", "type": "password", "required": true}, {"key": "sender_email", "label": "Default sender email", "type": "email", "placeholder": "news@example.com", "help": "From address used when a send does not supply its own."}, {"key": "sender_name", "label": "Default sender name", "type": "text", "placeholder": "Unified"}, {"key": "webhook_secret", "label": "Webhook secret / signing key", "type": "password", "help": "Secret used to verify incoming webhooks from this provider."}, {"key": "test_mode", "label": "Test mode (sandbox)", "type": "checkbox", "help": "When on, mail is recorded as a simulated delivery and never sent."}]}, {"id": "mailchimp", "name": "Mailchimp Transactional (Mandrill)", "logo": "email-mailchimp", "note": "Transactional mail from Mailchimp (Mandrill).", "fields": [{"key": "api_key", "label": "API key", "type": "password", "required": true}, {"key": "webhook_url", "label": "Webhook URL", "type": "url", "placeholder": "https://unified.example/webhooks/mandrill"}, {"key": "sender_email", "label": "Default sender email", "type": "email", "placeholder": "news@example.com", "help": "From address used when a send does not supply its own."}, {"key": "sender_name", "label": "Default sender name", "type": "text", "placeholder": "Unified"}, {"key": "webhook_secret", "label": "Webhook secret / signing key", "type": "password", "help": "Secret used to verify incoming webhooks from this provider."}, {"key": "test_mode", "label": "Test mode (sandbox)", "type": "checkbox", "help": "When on, mail is recorded as a simulated delivery and never sent."}]}, {"id": "brevo", "name": "Brevo (Sendinblue)", "logo": "email-brevo", "note": "Transactional mail and marketing from Brevo.", "fields": [{"key": "api_key", "label": "API key", "type": "password", "required": true}, {"key": "sender_email", "label": "Default sender email", "type": "email", "placeholder": "news@example.com", "help": "From address used when a send does not supply its own."}, {"key": "sender_name", "label": "Default sender name", "type": "text", "placeholder": "Unified"}, {"key": "webhook_secret", "label": "Webhook secret / signing key", "type": "password", "help": "Secret used to verify incoming webhooks from this provider."}, {"key": "test_mode", "label": "Test mode (sandbox)", "type": "checkbox", "help": "When on, mail is recorded as a simulated delivery and never sent."}]}, {"id": "sparkpost", "name": "SparkPost", "logo": "email-sparkpost", "note": "Transactional mail with a US or EU region.", "fields": [{"key": "api_key", "label": "API key", "type": "password", "required": true}, {"key": "region", "label": "Region", "type": "select", "options": [["us", "US"], ["eu", "EU"]]}, {"key": "sender_email", "label": "Default sender email", "type": "email", "placeholder": "news@example.com", "help": "From address used when a send does not supply its own."}, {"key": "sender_name", "label": "Default sender name", "type": "text", "placeholder": "Unified"}, {"key": "webhook_secret", "label": "Webhook secret / signing key", "type": "password", "help": "Secret used to verify incoming webhooks from this provider."}, {"key": "test_mode", "label": "Test mode (sandbox)", "type": "checkbox", "help": "When on, mail is recorded as a simulated delivery and never sent."}]}];

  /* The default builtin: local sendmail, like PHP mail(). */
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
