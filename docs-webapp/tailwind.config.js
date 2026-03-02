/** @type {import('tailwindcss').Config} */
export default {
  content: ['./index.html', './src/**/*.{js,ts,jsx,tsx}'],
  theme: {
    extend: {
      colors: {
        docs: {
          bg: '#0b1020',
          panel: '#111a33',
          panelSoft: '#172445',
          border: '#263866',
          text: '#e6ebff',
          muted: '#9eb0df',
          accent: '#4f9dff',
          accentSoft: '#8dc0ff',
        },
      },
      boxShadow: {
        glow: '0 0 0 1px rgba(79,157,255,0.2), 0 10px 30px rgba(6,15,35,0.45)',
      },
    },
  },
  plugins: [],
}

