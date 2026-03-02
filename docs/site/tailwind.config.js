/** @type {import('tailwindcss').Config} */
export default {
  content: ['./index.html', './src/**/*.{js,ts,jsx,tsx}'],
  theme: {
    extend: {
      colors: {
        rust: {
          bg: '#15120f',
          panel: '#231a15',
          panelSoft: '#2f241d',
          border: '#5f4635',
          text: '#f4ebe3',
          muted: '#cdb8a7',
          accent: '#c86a2b',
          accentSoft: '#e9a679',
        },
      },
      boxShadow: {
        glow: '0 0 0 1px rgba(200,106,43,0.24), 0 14px 34px rgba(0,0,0,0.35)',
      },
    },
  },
  plugins: [],
}

