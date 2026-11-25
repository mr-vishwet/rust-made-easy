// Vite config for the fixture: a distinctive dev port (5199) so the
// golden test can prove e2r read the port from the config, not the
// Vite default (5173).
export default {
  server: { port: 5199 },
  build: { outDir: 'dist' },
};
