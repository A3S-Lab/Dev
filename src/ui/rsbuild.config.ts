import { defineConfig } from '@rsbuild/core';
import { pluginReact } from '@rsbuild/plugin-react';
import fs from 'node:fs';

export default defineConfig({
  plugins: [pluginReact()],

  html: {
    template: './index.html',
    title: 'a3s dev',
    inject: 'body',
  },

  output: {
    inlineScripts: true,
    inlineStyles: true,
  },

  tools: {
    rspack: {
      optimization: {
        splitChunks: false,
        runtimeChunk: false,
      },
    },
  },

  server: {
    port: 13500,
    proxy: {
      '/api': {
        target: 'http://localhost:10350',
        changeOrigin: true,
      },
    },
  },

  hooks: {
    onAfterBuild() {
      // rsbuild forces `defer` on inline scripts; remove it so the single
      // HTML file works correctly when served without a module bundler.
      const out = 'dist/index.html';
      const html = fs.readFileSync(out, 'utf8');
      fs.writeFileSync(out, html.replace(/<script defer>/g, '<script>'));
    },
  },
});
