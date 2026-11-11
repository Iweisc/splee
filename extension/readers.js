// Reads the tab the popup opened on, and nothing else: only what's already in its DOM, with no requests and no other pages.
// The popup injects selectors.js and this file, then calls spleeRead. The tests run both in a vm with a parsed fixture.
(() => {
  const S = globalThis.SPLEE_SELECTORS;
  const EMAIL = /^[^\s@<>"',;]+@[a-z0-9-]+(\.[a-z0-9-]+)+$/;
  const SIZE = /(\d[\d,]*\s*(?:-|–|to)\s*\d[\d,]*|\d[\d,]*\+)\s*employees/i;
  const MAX_EMAILS = 10;

  const clean = (s) => (s || "").replace(/\s+/g, " ").trim();

  // LinkedIn repeats text in a visually hidden span for screen readers. The aria-hidden copy is the one on screen.
  function text(el) {
    if (!el) return "";
    const shown = el.querySelector("[aria-hidden='true']");
    return clean((shown || el).textContent);
  }

  function firstText(doc, selectors) {
    for (const s of selectors) {
      for (const el of doc.querySelectorAll(s)) {
        const t = text(el);
        if (t) return t;
      }
    }
    return "";
  }

  function meta(doc, name) {
    return clean(doc.querySelector(`meta[property="${name}"], meta[name="${name}"]`)?.getAttribute("content"));
  }

  // "Sam Lee - Fieldnote | LinkedIn", which is what LinkedIn shows signed out.
  function linkedinTitle(doc) {
    return (meta(doc, "og:title") || doc.title || "").replace(/\s*\|\s*LinkedIn\s*$/i, "");
  }

  // <dt>Website</dt><dd>…</dd> in a company's About section.
  function definition(doc, label) {
    for (const dt of doc.querySelectorAll("dt")) {
      if (clean(dt.textContent).toLowerCase() !== label.toLowerCase()) continue;
      let dd = dt.nextElementSibling;
      while (dd && dd.tagName !== "DD") dd = dd.nextElementSibling;
      if (dd) return dd;
    }
    return null;
  }

  // LinkedIn sends outside links through linkedin.com/redir/redirect?url=…
  function outsideLink(href, base) {
    try {
      const url = new URL(href, base);
      if (/(^|\.)linkedin\.com$/.test(url.hostname)) return url.searchParams.get("url") || "";
      return /^https?:$/.test(url.protocol) ? url.href : "";
    } catch {
      return "";
    }
  }

  function domainOf(input) {
    const raw = clean(input).toLowerCase();
    if (!raw) return "";
    try {
      return new URL(raw.includes("://") ? raw : `https://${raw}`).hostname.replace(/^www\./, "");
    } catch {
      return "";
    }
  }

  // "Head of Growth at Fieldnote | Ex-Stripe" is the title "Head of Growth" at the company "Fieldnote".
  function splitHeadline(headline) {
    const main = clean(headline.split(/\s+[|•·]\s+/)[0]);
    const m = main.match(/^(.+?)\s+(?:at|@)\s+(.+)$/i);
    return m ? [clean(m[1]), clean(m[2])] : [main, ""];
  }

  function readProfile(doc, url) {
    const P = S.profile;
    const parts = linkedinTitle(doc).split(" - ").map(clean).filter(Boolean);
    const name = firstText(doc, P.name) || parts[0] || "";
    const [title, headlineCompany] = splitHeadline(firstText(doc, P.headline));
    let company = "";
    for (const s of P.currentCompanyLabel) {
      const label = doc.querySelector(s)?.getAttribute("aria-label") || "";
      const m = label.match(/^Current company:\s*(.+?)\.?(?:\s+Click\b.*)?$/i);
      if (m) {
        company = clean(m[1]);
        break;
      }
    }
    company ||= firstText(doc, P.currentCompany) || headlineCompany || (parts.length > 1 ? parts[parts.length - 1] : "");
    const id = url.pathname.match(/^\/in\/([^/]+)/)[1];
    return { kind: "person", name, title, company, linkedinUrl: `https://www.linkedin.com/in/${id}/` };
  }

  function readCompany(doc, url) {
    const C = S.company;
    const name = firstText(doc, C.name) || clean(linkedinTitle(doc).split(/:\s/)[0]);

    let website = "";
    const dd = definition(doc, C.websiteLabel);
    if (dd) {
      const a = dd.querySelector("a[href]");
      website = (a && outsideLink(a.getAttribute("href"), url.href)) || text(dd);
    }
    for (const s of C.website) {
      if (website) break;
      const el = doc.querySelector(s);
      if (el) website = (el.getAttribute("href") && outsideLink(el.getAttribute("href"), url.href)) || text(el);
    }

    let size = text(definition(doc, C.sizeLabel)).match(SIZE)?.[1] || "";
    for (const s of C.size) {
      if (size) break;
      for (const el of doc.querySelectorAll(s)) {
        size = text(el).match(SIZE)?.[1] || "";
        if (size) break;
      }
    }
    size = size.replace(/\s+/g, "").replace("–", "-");
    const slug = url.pathname.match(/^\/company\/([^/]+)/)[1];
    return { kind: "company", name, domain: domainOf(website), size, sourceUrl: `https://www.linkedin.com/company/${slug}/` };
  }

  // "Pricing | Fieldnote" and "Fieldnote - Home" are both Fieldnote. The part that matches the domain wins.
  function siteName(title, domain) {
    const root = domain.split(".")[0];
    const parts = clean(title).split(/\s*[|–—·•]\s*|\s+-\s+|:\s+/).map(clean).filter(Boolean);
    const squash = (s) => s.toLowerCase().replace(/[^a-z0-9]/g, "");
    const match = parts.find((p) => squash(p) && (squash(p).includes(root) || root.includes(squash(p))));
    if (match) return match;
    if (parts.length > 1) return parts.reduce((a, b) => (b.length < a.length ? b : a));
    return parts[0] || root.charAt(0).toUpperCase() + root.slice(1);
  }

  // Addresses at the site's own domain come first, since they're the company's.
  function mailtos(doc, domain) {
    const found = [];
    for (const a of doc.querySelectorAll("a[href]")) {
      const href = a.getAttribute("href").trim();
      if (!/^mailto:/i.test(href)) continue;
      let to = href.slice(7).split("?")[0];
      try {
        to = decodeURIComponent(to);
      } catch {
        continue;
      }
      for (const raw of to.split(",")) {
        const email = raw.trim().toLowerCase();
        if (EMAIL.test(email) && !found.includes(email)) found.push(email);
      }
    }
    const own = (e) => {
      const d = e.split("@")[1];
      return d === domain || d.endsWith(`.${domain}`) || domain.endsWith(`.${d}`);
    };
    return [...found.filter(own), ...found.filter((e) => !own(e))].slice(0, MAX_EMAILS);
  }

  function readSite(doc, url) {
    const domain = url.hostname.replace(/^www\./, "");
    const name = meta(doc, "og:site_name") || meta(doc, "application-name") || siteName(doc.title || "", domain);
    return { kind: "site", name, domain, emails: mailtos(doc, domain), sourceUrl: url.href };
  }

  globalThis.spleeRead = (doc, href) => {
    const url = new URL(href);
    if (!/^https?:$/.test(url.protocol)) return { kind: "none" };
    if (/(^|\.)linkedin\.com$/.test(url.hostname)) {
      if (/^\/in\/[^/]+/.test(url.pathname)) return readProfile(doc, url);
      if (/^\/company\/[^/]+/.test(url.pathname)) return readCompany(doc, url);
      return { kind: "none" };
    }
    return readSite(doc, url);
  };
})();
