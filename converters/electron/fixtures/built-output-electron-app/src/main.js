// Renderer source entry (fixture): the source `vite build` bundles.
import './style.css';

document.getElementById('version')?.addEventListener('click', async () => {
  const version = await window.api.getVersion();
  console.log('version', version);
});
