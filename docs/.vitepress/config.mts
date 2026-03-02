import { defineConfig } from 'vitepress'

export default defineConfig({
  title: 'otter',
  description: 'Rust orchestration service documentation',
  lastUpdated: true,
  cleanUrls: true,
  themeConfig: {
    search: {
      provider: 'local'
    },
    outline: {
      level: [2, 3],
      label: 'On this page'
    },
    editLink: {
      pattern: 'https://github.com/Unchained-Labs/otter/edit/main/docs/:path',
      text: 'Edit this page on GitHub'
    },
    docFooter: {
      prev: 'Previous',
      next: 'Next'
    },
    footer: {
      message: 'otter docs',
      copyright: 'Unchained Labs'
    },
    nav: [
      { text: 'Home', link: '/' },
      { text: 'Getting Started', link: '/tutorials/getting-started' },
      { text: 'Architecture', link: '/architecture' },
      { text: 'API', link: '/api' },
      { text: 'Tutorials', link: '/tutorials/getting-started' },
      { text: 'Operations', link: '/runbook' },
      { text: 'References', link: '/related-documents' }
    ],
    sidebar: [
      {
        text: 'Overview',
        collapsed: false,
        items: [
          { text: 'Product Landing', link: '/' },
          { text: 'Architecture', link: '/architecture' },
          { text: 'Prompt-to-Result Flow', link: '/prompt-to-result-flow' },
          { text: 'Concepts', link: '/concepts' }
        ]
      },
      {
        text: 'Tutorials',
        collapsed: false,
        items: [
          { text: 'Getting Started', link: '/tutorials/getting-started' },
          { text: 'Runtime Operations', link: '/tutorials/runtime-operations' }
        ]
      },
      {
        text: 'API and Interfaces',
        collapsed: false,
        items: [
          { text: 'REST API', link: '/api' },
          { text: 'API Guide', link: '/api-guide' }
        ]
      },
      {
        text: 'Operations',
        collapsed: false,
        items: [
          { text: 'Runbook', link: '/runbook' },
          { text: 'NUC Operations', link: '/operations-nuc' },
          { text: 'Workspace Trust Model', link: '/workspace-trust-model' }
        ]
      },
      {
        text: 'References',
        collapsed: false,
        items: [{ text: 'Related Documents', link: '/related-documents' }]
      }
    ],
    socialLinks: [{ icon: 'github', link: 'https://github.com/Unchained-Labs/otter' }]
  }
})
