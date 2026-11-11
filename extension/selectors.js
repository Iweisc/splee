// LinkedIn changes its markup often, so every selector the readers use lives here, best first. When a field stops reading,
// add the new selector at the top of its list and keep the old ones for pages that still have them.
globalThis.SPLEE_SELECTORS = {
  profile: {
    name: ["main section h1", "h1.text-heading-xlarge", ".pv-top-card h1", ".top-card-layout__title", "h1"],
    headline: [
      "main section .text-body-medium.break-words",
      ".pv-text-details__left-panel .text-body-medium",
      ".pv-top-card .text-body-medium",
      ".top-card-layout__headline",
    ],
    // "Current company: Fieldnote. Click to skip to experience card"
    currentCompanyLabel: ['button[aria-label^="Current company"]', '[aria-label^="Current company"]'],
    currentCompany: [
      ".pv-text-details__right-panel li span[aria-hidden='true']",
      ".pv-text-details__right-panel li",
      '[data-section="currentPositionsDetails"] .top-card-link__description',
      ".top-card-link--link span",
    ],
  },
  company: {
    name: ["main h1", ".org-top-card-summary__title", ".top-card-layout__title", "h1"],
    // A <dt> with this text, and its <dd>, in the About section.
    websiteLabel: "Website",
    sizeLabel: "Company size",
    website: [
      '[data-test-id="about-us__website"] a',
      ".org-top-card-primary-actions__inner a[href^='http']",
      ".org-about-us-company-module__website",
      "a.top-card-layout__cta",
    ],
    size: [
      '[data-test-id="about-us__size"] dd',
      ".org-top-card-summary-info-list__info-item",
      ".org-about-company-module__company-size-definition-text",
      ".top-card-layout__first-subline",
    ],
  },
};
