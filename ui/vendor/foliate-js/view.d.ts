// Types for the parts of foliate-js's <foliate-view> the Taisce reader uses
// (written for Taisce; upstream ships no types).

export interface TocItem {
  label: string
  href: string
  subitems?: TocItem[] | null
}

export interface FoliateSection {
  id: string
  linear?: string
  cfi?: string
  createDocument?: () => Promise<Document>
}

export interface FoliateBook {
  metadata?: { title?: string | Record<string, string>; author?: unknown; language?: string | string[] }
  toc?: TocItem[]
  sections: FoliateSection[]
  dir?: string
  transformTarget?: EventTarget
}

export interface RelocateDetail {
  fraction: number
  section?: { current: number; total: number }
  location?: { current: number; next: number; total: number }
  time?: { section: number; total: number }
  tocItem?: TocItem | null
  pageItem?: unknown
  cfi: string
  range: Range
  reason?: string
}

export interface SearchExcerpt {
  pre: string
  match: string
  post: string
}

export type SearchResult =
  | 'done'
  | { progress: number }
  | { label: string; subitems: { cfi: string; excerpt: SearchExcerpt }[] }

export interface Paginator extends HTMLElement {
  goTo(target: { index: number; anchor?: (doc: Document) => Range | Element | number | null }): Promise<void>
  setStyles(css: string | [string, string]): void
  next(): Promise<void>
  prev(): Promise<void>
  getContents(): { doc: Document; index: number }[]
  destroy(): void
}

export declare class View extends HTMLElement {
  book: FoliateBook
  renderer: Paginator
  lastLocation: RelocateDetail | null
  open(book: File | Blob | string): Promise<void>
  close(): void
  init(opts: { lastLocation?: string | null; showTextStart?: boolean }): Promise<void>
  goTo(target: string | number): Promise<unknown>
  goToFraction(frac: number): Promise<void>
  goLeft(): Promise<void>
  goRight(): Promise<void>
  prev(distance?: number): Promise<void>
  next(distance?: number): Promise<void>
  getSectionFractions(): number[]
  search(opts: { query: string; matchCase?: boolean; matchDiacritics?: boolean; matchWholeWords?: boolean; index?: number }): AsyncGenerator<SearchResult>
  clearSearch(): void
}

export declare const makeBook: (file: File | Blob | string) => Promise<FoliateBook>
