//! The tools a company's homepage and DNS give away: script and link hosts, the meta generator, response headers and
//! cookies, markers in the HTML, MX hosts, SPF includes and the TXT records tools ask a domain to publish to prove it's
//! theirs. Matching is plain string comparison over one fetched page and two DNS lookups, so it costs no tokens.
//!
//! A tool's name is what tech_scans and project_tech_picks store, so renaming one needs a migration that renames it there.

use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Category {
    Crm,
    Marketing,
    EmailSending,
    Support,
    Analytics,
    SalesIntelligence,
    Advertising,
    Ecommerce,
    Cms,
    Payments,
    Hosting,
    Scheduling,
    Email,
    Workplace,
}

impl Category {
    pub const ALL: [Category; 14] = [
        Category::Crm,
        Category::Marketing,
        Category::EmailSending,
        Category::Support,
        Category::Analytics,
        Category::SalesIntelligence,
        Category::Advertising,
        Category::Ecommerce,
        Category::Cms,
        Category::Payments,
        Category::Hosting,
        Category::Scheduling,
        Category::Email,
        Category::Workplace,
    ];

    pub fn key(self) -> &'static str {
        match self {
            Category::Crm => "crm",
            Category::Marketing => "marketing",
            Category::EmailSending => "email_sending",
            Category::Support => "support",
            Category::Analytics => "analytics",
            Category::SalesIntelligence => "sales_intelligence",
            Category::Advertising => "advertising",
            Category::Ecommerce => "ecommerce",
            Category::Cms => "cms",
            Category::Payments => "payments",
            Category::Hosting => "hosting",
            Category::Scheduling => "scheduling",
            Category::Email => "email",
            Category::Workplace => "workplace",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Category::Crm => "CRM",
            Category::Marketing => "Marketing automation",
            Category::EmailSending => "Email delivery",
            Category::Support => "Chat and support",
            Category::Analytics => "Analytics",
            Category::SalesIntelligence => "Sales intelligence",
            Category::Advertising => "Advertising",
            Category::Ecommerce => "Ecommerce",
            Category::Cms => "Website builder",
            Category::Payments => "Payments",
            Category::Hosting => "Hosting and CDN",
            Category::Scheduling => "Scheduling",
            Category::Email => "Email provider",
            Category::Workplace => "Workplace tools",
        }
    }

    pub fn parse(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|c| c.key() == key)
    }
}

/// What a tool leaves behind. Every value is lowercase.
#[derive(Debug, Clone, Copy)]
pub enum Rule {
    /// A src or href URL on this host or a subdomain of it, optionally under a path: "js.stripe.com", "googletagmanager.com/gtm.js".
    Src(&'static str),
    /// Text anywhere in the page, for inline snippets like "window.intercomsettings".
    Html(&'static str),
    /// The start of the meta generator, like "wordpress".
    Generator(&'static str),
    /// A response header by name whose value contains the second string. An empty one matches any value.
    Header(&'static str, &'static str),
    /// The start of a cookie's name.
    Cookie(&'static str),
    /// An MX host on this domain.
    Mx(&'static str),
    /// The start of a TXT record on the domain itself.
    Txt(&'static str),
    /// A domain the SPF record includes, or a subdomain of it.
    Spf(&'static str),
}

use Rule::*;

#[derive(Debug, Serialize)]
pub struct Tech {
    pub name: &'static str,
    pub category: Category,
    #[serde(skip)]
    pub rules: &'static [Rule],
}

const fn tech(name: &'static str, category: Category, rules: &'static [Rule]) -> Tech {
    Tech { name, category, rules }
}

use Category::*;

pub static CATALOGUE: &[Tech] = &[
    tech("HubSpot", Crm, &[
        Src("js.hs-scripts.com"), Src("js.hsforms.net"), Src("js.hs-analytics.net"), Src("js.hscollectedforms.net"), Src("js.hsadspixel.net"),
        Src("js.usemessages.com"), Cookie("hubspotutk"), Cookie("__hstc"), Txt("hubspot-developer-verification"), Txt("hubspot-domain-verification"),
    ]),
    tech("Salesforce", Crm, &[
        Src("force.com"), Src("salesforceliveagent.com"), Html("servlet.webtolead"), Html("embeddedservice_bootstrap"), Spf("_spf.salesforce.com"),
    ]),
    tech("Pipedrive", Crm, &[Src("leadbooster-chat.pipedrive.com"), Src("webforms.pipedrive.com"), Html("pipedrivewebforms")]),
    tech("Zoho CRM", Crm, &[Src("crm.zoho.com"), Src("crm.zoho.eu"), Src("crm.zoho.in"), Src("zohopublic.com"), Html("crm.zoho.com/crm/webtoleadform")]),
    tech("Microsoft Dynamics 365", Crm, &[Src("dynamics.com"), Html("msdynmkt"), Html("d365mktformloader")]),
    tech("Freshsales", Crm, &[Src("freshsales.io"), Src("myfreshworks.com/crm")]),
    tech("Keap", Crm, &[Src("infusionsoft.com"), Src("infusionsoft.app"), Src("keap.app")]),
    tech("Marketo", Marketing, &[Src("munchkin.marketo.net"), Src("marketo.com"), Src("mktoweb.com"), Cookie("_mkto_trk"), Html("mktoform")]),
    tech("Pardot", Marketing, &[Src("pi.pardot.com"), Src("go.pardot.com"), Html("pi.pardot.com"), Html("piaid =")]),
    tech("ActiveCampaign", Marketing, &[Src("trackcmp.net"), Src("activehosted.com"), Html("trackcmp.net"), Spf("emsd1.com")]),
    tech("Mailchimp", Marketing, &[Src("chimpstatic.com"), Src("list-manage.com"), Spf("servers.mcsv.net")]),
    tech("Klaviyo", Marketing, &[Src("static.klaviyo.com"), Src("static-tracking.klaviyo.com"), Html("klaviyo.com/onsite")]),
    tech("Customer.io", Marketing, &[Src("assets.customer.io"), Html("assets.customer.io"), Spf("customeriomail.com")]),
    tech("Brevo", Marketing, &[Src("sibautomation.com"), Src("sibforms.com"), Src("brevo.com"), Txt("brevo-code"), Txt("sendinblue-code"), Spf("spf.brevo.com"), Spf("spf.sendinblue.com")]),
    tech("MailerLite", Marketing, &[Src("mailerlite.com"), Src("mlcdn.com")]),
    tech("Kit", Marketing, &[Src("convertkit.com"), Src("ck.page"), Src("kit.com")]),
    tech("Constant Contact", Marketing, &[Src("ctctcdn.com"), Src("constantcontact.com"), Spf("spf.constantcontact.com")]),
    tech("Oracle Eloqua", Marketing, &[Src("en25.com"), Src("eloqua.com"), Html("elqcfg.min.js")]),
    tech("Braze", Marketing, &[Src("js.appboycdn.com"), Src("braze.com")]),
    tech("Iterable", Marketing, &[Src("js.iterable.com")]),
    tech("Drip", Marketing, &[Src("tag.getdrip.com")]),
    tech("Omnisend", Marketing, &[Src("omnisnippet1.com"), Src("omnisrc.com")]),
    tech("GetResponse", Marketing, &[Src("getresponse.com"), Src("gr-cdn.com")]),
    tech("Mautic", Marketing, &[Html("/mtc.js"), Html("mauticform")]),
    tech("Wistia", Marketing, &[Src("fast.wistia.com"), Src("fast.wistia.net")]),
    tech("Vidyard", Marketing, &[Src("play.vidyard.com"), Src("embed.vidyard.com")]),
    tech("Typeform", Marketing, &[Src("embed.typeform.com"), Src("form.typeform.com")]),
    tech("SendGrid", EmailSending, &[Spf("sendgrid.net")]),
    tech("Mailgun", EmailSending, &[Spf("mailgun.org")]),
    tech("Amazon SES", EmailSending, &[Spf("amazonses.com")]),
    tech("Postmark", EmailSending, &[Spf("spf.mtasv.net")]),
    tech("Mandrill", EmailSending, &[Spf("spf.mandrillapp.com")]),
    tech("SparkPost", EmailSending, &[Spf("sparkpostmail.com")]),
    tech("Intercom", Support, &[Src("widget.intercom.io"), Src("js.intercomcdn.com"), Html("window.intercomsettings"), Cookie("intercom-id-"), Cookie("intercom-session-")]),
    tech("Drift", Support, &[Src("js.driftt.com"), Html("js.driftt.com")]),
    tech("Zendesk", Support, &[Src("static.zdassets.com"), Src("v2.zopim.com"), Src("zendesk.com"), Txt("zendeskverification"), Spf("mail.zendesk.com")]),
    tech("Freshdesk", Support, &[Src("freshdesk.com"), Src("widget.freshworks.com"), Spf("email.freshdesk.com")]),
    tech("Freshchat", Support, &[Src("wchat.freshchat.com"), Src("freshchat.com")]),
    tech("Crisp", Support, &[Src("client.crisp.chat"), Html("client.crisp.chat")]),
    tech("Help Scout", Support, &[Src("beacon-v2.helpscout.net"), Html("beacon-v2.helpscout.net"), Spf("helpscoutemail.com")]),
    tech("LiveChat", Support, &[Src("cdn.livechatinc.com"), Html("__lc.license")]),
    tech("Tawk.to", Support, &[Src("embed.tawk.to"), Html("embed.tawk.to")]),
    tech("Tidio", Support, &[Src("code.tidio.co")]),
    tech("Olark", Support, &[Src("static.olark.com"), Html("static.olark.com")]),
    tech("Gorgias", Support, &[Src("gorgias.chat"), Src("gorgias.io")]),
    tech("Kustomer", Support, &[Src("cdn.kustomerapp.com")]),
    tech("Front", Support, &[Src("chat-assets.frontapp.com")]),
    tech("Qualified", Support, &[Src("js.qualified.com")]),
    tech("Zoho SalesIQ", Support, &[Src("salesiq.zoho.com"), Src("salesiq.zoho.eu"), Src("salesiq.zohopublic.com")]),
    tech("Smartsupp", Support, &[Src("smartsuppchat.com"), Html("smartsupp.com/loader")]),
    tech("JivoChat", Support, &[Src("code.jivosite.com"), Src("code.jivo.ru")]),
    tech("Chatwoot", Support, &[Html("chatwootsdk.run")]),
    tech("Google Analytics", Analytics, &[Src("googletagmanager.com/gtag/js"), Src("google-analytics.com"), Html("google-analytics.com/analytics.js"), Cookie("_ga")]),
    tech("Google Tag Manager", Analytics, &[Src("googletagmanager.com/gtm.js"), Html("googletagmanager.com/gtm.js"), Html("googletagmanager.com/ns.html")]),
    tech("Segment", Analytics, &[Src("cdn.segment.com"), Html("cdn.segment.com")]),
    tech("Mixpanel", Analytics, &[Src("cdn.mxpnl.com"), Html("mixpanel.init(")]),
    tech("Amplitude", Analytics, &[Src("cdn.amplitude.com"), Html("cdn.amplitude.com")]),
    tech("Heap", Analytics, &[Src("cdn.heapanalytics.com"), Html("heap.load(")]),
    tech("PostHog", Analytics, &[Src("posthog.com"), Html("posthog.init(")]),
    tech("Hotjar", Analytics, &[Src("static.hotjar.com"), Html("static.hotjar.com"), Cookie("_hjsession")]),
    tech("Microsoft Clarity", Analytics, &[Src("clarity.ms"), Html("clarity.ms/tag")]),
    tech("FullStory", Analytics, &[Src("fullstory.com"), Html("window['_fs_org']"), Html("_fs_org =")]),
    tech("Plausible", Analytics, &[Src("plausible.io")]),
    tech("Fathom", Analytics, &[Src("cdn.usefathom.com")]),
    tech("Matomo", Analytics, &[Src("matomo.cloud"), Html("matomo.js"), Html("piwik.js")]),
    tech("Adobe Experience Cloud", Analytics, &[Src("assets.adobedtm.com"), Src("omtrdc.net"), Src("2o7.net")]),
    tech("Crazy Egg", Analytics, &[Src("script.crazyegg.com")]),
    tech("Lucky Orange", Analytics, &[Src("luckyorange.com"), Src("luckyorange.net")]),
    tech("Mouseflow", Analytics, &[Src("cdn.mouseflow.com")]),
    tech("Pendo", Analytics, &[Src("cdn.pendo.io"), Html("cdn.pendo.io")]),
    tech("Optimizely", Analytics, &[Src("cdn.optimizely.com")]),
    tech("VWO", Analytics, &[Src("dev.visualwebsiteoptimizer.com"), Html("visualwebsiteoptimizer.com")]),
    tech("ZoomInfo", SalesIntelligence, &[Src("ws.zoominfo.com"), Src("js.zi-scripts.com"), Html("js.zi-scripts.com")]),
    tech("6sense", SalesIntelligence, &[Src("j.6sc.co"), Html("j.6sc.co")]),
    tech("Demandbase", SalesIntelligence, &[Src("tag.demandbase.com"), Html("tag.demandbase.com")]),
    tech("Clearbit", SalesIntelligence, &[Src("tag.clearbitscripts.com"), Src("x.clearbitjs.com")]),
    tech("Leadfeeder", SalesIntelligence, &[Src("sc.lfeeder.com"), Src("lftracker.leadfeeder.com"), Html("sc.lfeeder.com")]),
    tech("Meta Ads", Advertising, &[Src("connect.facebook.net"), Html("connect.facebook.net"), Html("fbq('init'"), Html("fbq(\"init\""), Txt("facebook-domain-verification")]),
    tech("Google Ads", Advertising, &[Src("googleadservices.com"), Html("googleads.g.doubleclick.net"), Html("googleadservices.com/pagead/conversion")]),
    tech("LinkedIn Insight Tag", Advertising, &[Src("snap.licdn.com"), Html("_linkedin_partner_id")]),
    tech("TikTok Pixel", Advertising, &[Src("analytics.tiktok.com"), Html("analytics.tiktok.com")]),
    tech("X Ads", Advertising, &[Src("static.ads-twitter.com"), Html("static.ads-twitter.com")]),
    tech("Microsoft Advertising", Advertising, &[Src("bat.bing.com"), Html("bat.bing.com")]),
    tech("Pinterest Tag", Advertising, &[Src("s.pinimg.com/ct"), Html("s.pinimg.com/ct/core.js")]),
    tech("Reddit Pixel", Advertising, &[Src("redditstatic.com/ads"), Html("redditstatic.com/ads")]),
    tech("Shopify", Ecommerce, &[Src("cdn.shopify.com"), Html("cdn.shopify.com"), Header("x-shopid", ""), Header("powered-by", "shopify"), Cookie("_shopify_")]),
    tech("WooCommerce", Ecommerce, &[Generator("woocommerce"), Html("wp-content/plugins/woocommerce/")]),
    tech("BigCommerce", Ecommerce, &[Src("bigcommerce.com"), Header("x-bc-storefront-request-id", "")]),
    tech("Magento", Ecommerce, &[Html("text/x-magento-init"), Html("mage/cookies"), Cookie("mage-cache-")]),
    tech("Salesforce Commerce Cloud", Ecommerce, &[Src("demandware.net"), Html("demandware.static")]),
    tech("PrestaShop", Ecommerce, &[Generator("prestashop"), Cookie("prestashop-")]),
    tech("Shopware", Ecommerce, &[Generator("shopware")]),
    tech("Ecwid", Ecommerce, &[Src("app.ecwid.com")]),
    tech("Recharge", Ecommerce, &[Src("rechargeapps.com"), Src("rechargecdn.com")]),
    tech("Yotpo", Ecommerce, &[Src("yotpo.com"), Src("yotpo.io")]),
    tech("WordPress", Cms, &[Generator("wordpress"), Html("/wp-content/"), Html("/wp-includes/"), Header("link", "api.w.org")]),
    tech("Webflow", Cms, &[Generator("webflow"), Html("data-wf-page"), Src("website-files.com")]),
    tech("Wix", Cms, &[Generator("wix.com"), Src("static.parastorage.com"), Src("static.wixstatic.com"), Header("x-wix-request-id", "")]),
    tech("Squarespace", Cms, &[Src("static1.squarespace.com"), Src("squarespace-cdn.com"), Header("server", "squarespace")]),
    tech("Framer", Cms, &[Generator("framer"), Src("framerusercontent.com"), Header("server", "framer")]),
    tech("Ghost", Cms, &[Generator("ghost"), Html("data-ghost=")]),
    tech("Drupal", Cms, &[Generator("drupal"), Header("x-generator", "drupal"), Html("drupal-settings-json")]),
    tech("Joomla", Cms, &[Generator("joomla")]),
    tech("HubSpot CMS", Cms, &[Header("x-hs-hub-id", ""), Html("hs-sites.com")]),
    tech("Contentful", Cms, &[Src("ctfassets.net")]),
    tech("Sanity", Cms, &[Src("cdn.sanity.io")]),
    tech("Weebly", Cms, &[Src("editmysite.com"), Generator("weebly")]),
    tech("GoDaddy Website Builder", Cms, &[Generator("starfield technologies")]),
    tech("Unbounce", Cms, &[Src("ubembed.com"), Src("unbounce.com")]),
    tech("Leadpages", Cms, &[Src("leadpages.net"), Src("leadpages.io")]),
    tech("Stripe", Payments, &[Src("js.stripe.com"), Txt("stripe-verification")]),
    tech("Paddle", Payments, &[Src("cdn.paddle.com"), Html("paddle.setup("), Html("paddle.initialize(")]),
    tech("Chargebee", Payments, &[Src("js.chargebee.com"), Html("chargebee.init(")]),
    tech("PayPal", Payments, &[Src("paypal.com/sdk/js"), Src("paypalobjects.com")]),
    tech("Braintree", Payments, &[Src("js.braintreegateway.com")]),
    tech("Recurly", Payments, &[Src("js.recurly.com")]),
    tech("Adyen", Payments, &[Src("checkoutshopper-live.adyen.com"), Src("checkoutshopper-test.adyen.com")]),
    tech("Klarna", Payments, &[Src("klarna.com"), Src("klarnaservices.com")]),
    tech("Afterpay", Payments, &[Src("js.afterpay.com"), Src("static.afterpay.com")]),
    tech("Lemon Squeezy", Payments, &[Src("assets.lemonsqueezy.com"), Src("app.lemonsqueezy.com")]),
    tech("Gumroad", Payments, &[Src("gumroad.com/js"), Html("gumroad.com/js/gumroad.js")]),
    tech("Cloudflare", Hosting, &[Header("server", "cloudflare"), Header("cf-ray", ""), Cookie("__cf_bm")]),
    tech("Vercel", Hosting, &[Header("server", "vercel"), Header("x-vercel-id", "")]),
    tech("Netlify", Hosting, &[Header("server", "netlify"), Header("x-nf-request-id", "")]),
    tech("Amazon CloudFront", Hosting, &[Header("x-amz-cf-id", ""), Header("via", "cloudfront")]),
    tech("Fastly", Hosting, &[Header("x-fastly-request-id", ""), Header("x-served-by", "cache-")]),
    tech("Akamai", Hosting, &[Header("server", "akamaighost"), Header("x-akamai-transformed", "")]),
    tech("Google Cloud", Hosting, &[Header("server", "google frontend")]),
    tech("Microsoft Azure", Hosting, &[Header("x-azure-ref", "")]),
    tech("Heroku", Hosting, &[Header("via", "vegur")]),
    tech("GitHub Pages", Hosting, &[Header("server", "github.com")]),
    tech("WP Engine", Hosting, &[Header("wpe-backend", ""), Header("x-powered-by", "wp engine")]),
    tech("Kinsta", Hosting, &[Header("x-kinsta-cache", "")]),
    tech("Fly.io", Hosting, &[Header("fly-request-id", "")]),
    tech("Render", Hosting, &[Header("rndr-id", "")]),
    tech("Calendly", Scheduling, &[Src("calendly.com"), Html("assets.calendly.com")]),
    tech("Chili Piper", Scheduling, &[Src("js.chilipiper.com"), Src("chilipiper.com"), Html("chilipiper.submit(")]),
    tech("Cal.com", Scheduling, &[Src("app.cal.com"), Src("cal.com"), Html("app.cal.com/embed")]),
    tech("SavvyCal", Scheduling, &[Src("embed.savvycal.com"), Src("savvycal.com")]),
    tech("Acuity Scheduling", Scheduling, &[Src("acuityscheduling.com"), Src("as.me")]),
    tech("YouCanBookMe", Scheduling, &[Src("youcanbook.me")]),
    tech("Google Workspace", Email, &[Mx("google.com"), Mx("googlemail.com"), Spf("_spf.google.com")]),
    tech("Microsoft 365", Email, &[Mx("mail.protection.outlook.com"), Txt("ms="), Spf("spf.protection.outlook.com")]),
    tech("Zoho Mail", Email, &[Mx("zoho.com"), Mx("zoho.eu"), Mx("zoho.in"), Mx("zoho.com.au"), Txt("zoho-verification")]),
    tech("Proton Mail", Email, &[Mx("protonmail.ch"), Txt("protonmail-verification")]),
    tech("Fastmail", Email, &[Mx("messagingengine.com")]),
    tech("GoDaddy Email", Email, &[Mx("secureserver.net")]),
    tech("Mimecast", Email, &[Mx("mimecast.com"), Spf("_netblocks.mimecast.com")]),
    tech("Proofpoint", Email, &[Mx("pphosted.com"), Mx("ppe-hosted.com")]),
    tech("Barracuda", Email, &[Mx("barracudanetworks.com")]),
    tech("Atlassian", Workplace, &[Txt("atlassian-domain-verification")]),
    tech("DocuSign", Workplace, &[Txt("docusign=")]),
    tech("Apple Business", Workplace, &[Txt("apple-domain-verification")]),
    tech("Slack", Workplace, &[Txt("slack-domain-verification")]),
    tech("Zoom", Workplace, &[Txt("zoom_verify_")]),
    tech("Dropbox", Workplace, &[Txt("dropbox-domain-verification")]),
    tech("Adobe", Workplace, &[Txt("adobe-idp-site-verification"), Txt("adobe-sign-verification")]),
    tech("Miro", Workplace, &[Txt("miro-verification")]),
    tech("Webex", Workplace, &[Txt("webexdomainverification"), Txt("cisco-ci-domain-verification")]),
    tech("Okta", Workplace, &[Txt("okta-verification")]),
    tech("Twilio", Workplace, &[Txt("twilio-domain-verification")]),
    tech("Canva", Workplace, &[Txt("canva-site-verification")]),
    tech("Smartsheet", Workplace, &[Txt("smartsheet-site-validation")]),
];

pub fn find(name: &str) -> Option<&'static Tech> {
    CATALOGUE.iter().find(|t| t.name.eq_ignore_ascii_case(name.trim()))
}

/// The tools a filter value stands for: one tool by name, or every tool of a category by its key. None when it's neither.
pub fn named(filter: &str) -> Option<Vec<&'static str>> {
    if let Some(category) = Category::parse(filter.trim()) {
        return Some(CATALOGUE.iter().filter(|t| t.category == category).map(|t| t.name).collect());
    }
    find(filter).map(|t| vec![t.name])
}

/// What one scan saw: the homepage as served and the domain's MX hosts and TXT records.
#[derive(Debug, Default)]
pub struct Evidence {
    pub html: String,
    /// Names lowercase. Every Set-Cookie is its own entry.
    pub headers: Vec<(String, String)>,
    pub mx: Vec<String>,
    pub txt: Vec<String>,
}

/// The tools the evidence shows, in catalogue order.
pub fn detect(evidence: &Evidence) -> Vec<&'static str> {
    let html = evidence.html.to_lowercase();
    let urls = urls(&html);
    let generators: Vec<&str> = generators(&html);
    let headers: Vec<(String, String)> = evidence.headers.iter().map(|(n, v)| (n.to_lowercase(), v.to_lowercase())).collect();
    let cookies: Vec<&str> =
        headers.iter().filter(|(n, _)| n == "set-cookie").map(|(_, v)| v.split('=').next().unwrap_or_default().trim()).collect();
    let mx: Vec<String> = evidence.mx.iter().map(|h| h.trim_end_matches('.').to_lowercase()).collect();
    let txt: Vec<String> = evidence.txt.iter().map(|t| t.trim().to_lowercase()).collect();
    let spf: Vec<&str> = txt
        .iter()
        .filter(|t| t.starts_with("v=spf1"))
        .flat_map(|t| t.split_whitespace())
        .map(|term| term.trim_start_matches(['+', '-', '~', '?']))
        .filter_map(|term| term.strip_prefix("include:").or_else(|| term.strip_prefix("redirect=")))
        .collect();
    let matches = |rule: &Rule| match *rule {
        Src(rule) => {
            let (host, path) = rule.split_once('/').map_or((rule, ""), |(h, p)| (h, p));
            urls.iter().any(|(h, p)| on_domain(h, host) && p.starts_with(path))
        }
        Html(text) => html.contains(text),
        Generator(start) => generators.iter().any(|g| g.starts_with(start)),
        Header(name, value) => headers.iter().any(|(n, v)| n == name && v.contains(value)),
        Cookie(start) => cookies.iter().any(|c| c.starts_with(start)),
        Mx(domain) => mx.iter().any(|h| on_domain(h, domain)),
        Txt(start) => txt.iter().any(|t| t.starts_with(start)),
        Spf(domain) => spf.iter().any(|d| on_domain(d, domain)),
    };
    CATALOGUE.iter().filter(|t| t.rules.iter().any(matches)).map(|t| t.name).collect()
}

fn on_domain(host: &str, domain: &str) -> bool {
    host == domain || host.strip_suffix(domain).is_some_and(|rest| rest.ends_with('.'))
}

/// The host and path (without its leading slash) of every absolute src and href in the page.
fn urls(html: &str) -> Vec<(&str, &str)> {
    let mut found = vec![];
    for attr in ["src=", "href="] {
        for (at, _) in html.match_indices(attr) {
            let Some(value) = attr_value(&html[at + attr.len()..]) else { continue };
            let value = value.strip_prefix("https:").or_else(|| value.strip_prefix("http:")).unwrap_or(value);
            let Some(rest) = value.strip_prefix("//") else { continue };
            let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
            let host = rest[..end].split(':').next().unwrap_or_default();
            let path = rest[end..].strip_prefix('/').unwrap_or_default();
            found.push((host, path));
        }
    }
    found
}

/// A quoted or bare attribute value at the start of `s`.
fn attr_value(s: &str) -> Option<&str> {
    let s = s.trim_start();
    match s.chars().next()? {
        quote @ ('"' | '\'') => s[1..].find(quote).map(|end| &s[1..=end]),
        _ => Some(&s[..s.find(|c: char| c.is_whitespace() || c == '>').unwrap_or(s.len())]),
    }
}

/// The content of every `<meta name="generator">`, whichever order its attributes come in.
fn generators(html: &str) -> Vec<&str> {
    html.match_indices("<meta")
        .filter_map(|(at, _)| {
            let tag = &html[at..at + html[at..].find('>').unwrap_or(html.len() - at)];
            let name = tag.find("name=").and_then(|i| attr_value(&tag[i + 5..]))?;
            if name != "generator" {
                return None;
            }
            tag.find("content=").and_then(|i| attr_value(&tag[i + 8..])).map(str::trim)
        })
        .collect()
}

/// "Uses HubSpot", "Uses HubSpot and Intercom", "Uses HubSpot, Intercom and Stripe". None for no tools.
pub fn fact_line(names: &[impl AsRef<str>]) -> Option<String> {
    let names: Vec<&str> = names.iter().map(AsRef::as_ref).collect();
    match names.as_slice() {
        [] => None,
        [one] => Some(format!("Uses {one}")),
        [rest @ .., last] => Some(format!("Uses {} and {last}", rest.join(", "))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn page(html: &str) -> Evidence {
        Evidence { html: html.into(), ..Evidence::default() }
    }

    #[test]
    fn the_catalogue_has_unique_names_and_every_category() {
        let mut names: Vec<String> = CATALOGUE.iter().map(|t| t.name.to_lowercase()).collect();
        names.sort();
        names.dedup();
        assert_eq!(names.len(), CATALOGUE.len());
        assert!(CATALOGUE.len() >= 120, "{}", CATALOGUE.len());
        for category in Category::ALL {
            assert!(CATALOGUE.iter().any(|t| t.category == category), "{category:?}");
        }
        // Rules compare against lowercased evidence, so an uppercase rule could never match.
        for t in CATALOGUE {
            for rule in t.rules {
                let values = match *rule {
                    Src(v) | Html(v) | Generator(v) | Cookie(v) | Mx(v) | Txt(v) | Spf(v) => vec![v],
                    Header(name, v) => vec![name, v],
                };
                assert!(values.iter().all(|v| *v == v.to_lowercase()), "{}: {rule:?}", t.name);
            }
        }
    }

    #[test]
    fn scripts_links_and_inline_snippets_are_matched() {
        let html = r#"<!doctype html><html><head>
            <META NAME="Generator" CONTENT="WordPress 6.6.2">
            <script async src="https://www.googletagmanager.com/gtag/js?id=G-ABC"></script>
            <script type="text/javascript" id="hs-script-loader" async defer src="//js.hs-scripts.com/4242.js"></script>
            <link rel='stylesheet' href='https://example.com/wp-content/themes/acme/style.css'>
            <script>window.intercomSettings = { app_id: "abc" };</script>
            <script src=https://js.stripe.com/v3></script>
            </head><body><a href="https://calendly.com/acme/intro">Book a call</a></body></html>"#;
        assert_eq!(detect(&page(html)), ["HubSpot", "Intercom", "Google Analytics", "WordPress", "Stripe", "Calendly"]);
    }

    #[test]
    fn lookalike_hosts_and_mentions_are_not_matched() {
        // A blog post that names tools and links to their marketing sites, on a host that only ends like one.
        let html = r#"<meta content="Hugo 0.120" name="generator">
            <p>We moved from HubSpot to Pipedrive and love Intercom.</p>
            <a href="https://www.hubspot.com/pricing">HubSpot</a>
            <script src="https://js.stripe.com.evil.io/v3"></script>
            <script src="https://mytoolkit.com/app.js"></script>
            <img src="/images/shopify-logo.png">"#;
        assert!(detect(&page(html)).is_empty(), "{:?}", detect(&page(html)));
    }

    #[test]
    fn headers_and_cookies_are_matched() {
        let evidence = Evidence {
            headers: vec![
                ("Server".into(), "cloudflare".into()),
                ("X-ShopId".into(), "123".into()),
                ("Set-Cookie".into(), "_shopify_y=abc; Path=/".into()),
                ("Set-Cookie".into(), "hubspotutk=1; Path=/".into()),
                ("X-Powered-By".into(), "Express".into()),
            ],
            ..Evidence::default()
        };
        assert_eq!(detect(&evidence), ["HubSpot", "Shopify", "Cloudflare"]);
        // The same words in other headers, or a cookie whose value names one, aren't a match.
        let evidence = Evidence {
            headers: vec![("Via".into(), "1.1 varnish".into()), ("Set-Cookie".into(), "session=hubspotutk".into())],
            ..Evidence::default()
        };
        assert!(detect(&evidence).is_empty());
    }

    #[test]
    fn mx_spf_and_verification_records_are_matched() {
        let evidence = Evidence {
            mx: vec!["aspmx.l.google.com.".into(), "alt1.aspmx.l.google.com".into()],
            txt: vec![
                "v=spf1 include:_spf.google.com include:sendgrid.net include:mail.zendesk.com ~all".into(),
                "atlassian-domain-verification=abc123".into(),
                "stripe-verification=0f1e".into(),
                "facebook-domain-verification=xyz".into(),
                "MS=ms12345678".into(),
            ],
            ..Evidence::default()
        };
        assert_eq!(
            detect(&evidence),
            ["SendGrid", "Zendesk", "Meta Ads", "Stripe", "Google Workspace", "Microsoft 365", "Atlassian"]
        );
        // Microsoft 365 by its MX, and records that only mention a tool somewhere in the middle don't count.
        let evidence = Evidence {
            mx: vec!["acme-com.mail.protection.outlook.com".into(), "mail.notgoogle.com".into()],
            txt: vec!["google-site-verification=abc".into(), "note: we left atlassian-domain-verification".into(), "v=spf1 ~include:spf.x.io -all".into()],
            ..Evidence::default()
        };
        assert_eq!(detect(&evidence), ["Microsoft 365"]);
    }

    #[test]
    fn filters_name_a_tool_or_a_category() {
        assert_eq!(named("hubspot").unwrap(), ["HubSpot"]);
        assert!(named("scheduling").unwrap().contains(&"Chili Piper"));
        assert!(named("nothing like it").is_none());
    }

    #[test]
    fn the_fact_line_lists_tools_in_plain_words() {
        assert_eq!(fact_line(&[] as &[&str]), None);
        assert_eq!(fact_line(&["HubSpot"]).unwrap(), "Uses HubSpot");
        assert_eq!(fact_line(&["HubSpot", "Intercom"]).unwrap(), "Uses HubSpot and Intercom");
        assert_eq!(fact_line(&["HubSpot", "Intercom", "Stripe"]).unwrap(), "Uses HubSpot, Intercom and Stripe");
    }
}
