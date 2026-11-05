// Add real customer results and quotes here, with their permission. Each landing section stays hidden while its array is empty.

export interface CaseResult {
  name: string;
  company: string;
  quote: string;
  hotLeads: number;
  costPerLead: string;
  avatarUrl?: string;
  linkedinUrl?: string;
}

export interface Testimonial {
  name: string;
  company: string;
  quote: string;
  avatarUrl?: string;
}

export const caseResults: CaseResult[] = [];

export const testimonials: Testimonial[] = [];
