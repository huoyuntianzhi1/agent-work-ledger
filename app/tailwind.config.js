/** @type {import('tailwindcss').Config} */
export default {
  content: ["./index.html", "./src/**/*.{js,ts,jsx,tsx}"],
  theme: {
    extend: {
      fontFamily: {
        sans: [
          "-apple-system",
          "BlinkMacSystemFont",
          "SF Pro Display",
          "Helvetica Neue",
          "Arial",
          "PingFang SC",
          "Microsoft YaHei",
          "sans-serif",
        ],
        mono: ["SF Mono", "Menlo", "monospace"],
      },
      colors: {
        ink: {
          50: "#f7f7f8",
          900: "#0a0a0c",
        },
      },
    },
  },
  plugins: [],
};
