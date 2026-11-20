import parser from "@babel/eslint-parser";
import hooks from "eslint-plugin-react-hooks";

export default [
  {
    files: ["src/**/*.{ts,tsx}"],
    languageOptions: {
      parser,
      parserOptions: { requireConfigFile: false, babelOptions: { parserOpts: { plugins: ["typescript"] } } },
    },
    plugins: { "react-hooks": hooks },
    rules: {
      "react-hooks/rules-of-hooks": "error",
      "react-hooks/exhaustive-deps": "error",
    },
  },
  { files: ["src/**/*.tsx"], languageOptions: { parserOptions: { babelOptions: { parserOpts: { plugins: ["typescript", "jsx"] } } } } },
];
