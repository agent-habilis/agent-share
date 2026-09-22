export default {
  index: {
    type: 'page',
    title: 'Home',
    display: 'hidden',
    theme: {
      layout: 'full',
      sidebar: false,
      toc: false,
      breadcrumb: false,
      pagination: false,
      timestamp: false,
      copyPage: false,
    },
  },
  docs: { type: 'page', title: 'Docs' },
  // The webapp is not part of this Next build: it is a separate Bun bundle that
  // scripts/build.ts puts under dist/app/. An href, so the navbar does a full
  // document load into it instead of a soft navigation Next cannot serve.
  app: { type: 'page', title: 'Webapp', href: '/app/' },
}
