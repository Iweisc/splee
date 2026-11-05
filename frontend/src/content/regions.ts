export interface Region {
  slug: string;
  name: string;
}

// Slugs match explee.com's /b2b-database/locations/* URLs. The name doubles as the explorer's country filter.
export const regions: Region[] = [
  { slug: "united-states", name: "United States" },
  { slug: "north-america", name: "North America" },
  { slug: "europe", name: "Europe" },
  { slug: "asia-pacific", name: "Asia Pacific" },
  { slug: "southeast-asia", name: "Southeast Asia" },
  { slug: "latin-america", name: "Latin America" },
  { slug: "middle-east", name: "Middle East" },
  { slug: "cis", name: "CIS Countries" },
  { slug: "oceania", name: "Oceania" },
  { slug: "africa", name: "Africa" },
];
