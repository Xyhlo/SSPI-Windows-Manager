/** @type {import('tailwindcss').Config} */
export default {
  content: [
    "./index.html",
    "./src/**/*.{js,ts,jsx,tsx}",
  ],
  theme: {
    extend: {
      colors: {
        'ps-blue': '#e4e4e1',
        'ps-blue-glow': 'rgba(228, 228, 225, 0.14)',
        'ps-black': '#0b0b0c',
        'ps-surface': '#111113',
        'ps-card': '#161618',
        'ps-border': 'rgba(255, 255, 255, 0.08)',
      },
      borderRadius: {
        'ps-xl': '0.625rem',
        'ps-2xl': '0.75rem',
        'ps-3xl': '0.875rem',
      },
      fontFamily: {
        'ps5': ["Geist", "Segoe UI", "Helvetica Neue", "Arial", "sans-serif"],
      }
    },
  },
  plugins: [],
}
