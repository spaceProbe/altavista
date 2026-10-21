// Question 264: the server's verdict on the Blue Marble basemap, as the viewer reads it.
//
// altavista/server.py checks the tile pyramid once at start-up and stamps the answer on every
// scenario's `imagery` for a profile whose `urlTemplate` points at `/imagery/bluemarble/`:
// `installed: true`, or `installed: false` with a `notice` naming the directory and the build
// command. A scenario whose imagery carries no `installed` key (a fixture or third-party
// template, or an older server) is not judged: only an explicit `false` withholds the globe's
// tiles and the 2D map's mosaic, because requesting a pyramid that is not there would just be
// thousands of 404s. There is no fallback to the test colours; the plain body stays, and the
// panels say why. Pure functions, no DOM: web/js/imagery_status_check.mjs drives them.

const GENERIC_NOTICE = 'Blue Marble basemap not installed (the server found no complete tile pyramid).';

/** True only when the server said, explicitly, that the basemap is not installed. */
export function basemapNotInstalled(imagery) {
  return !!imagery && imagery.installed === false;
}

/** The sentence to show instead of a basemap, or null when the basemap is usable. */
export function basemapNotice(imagery) {
  if (!basemapNotInstalled(imagery)) return null;
  return typeof imagery.notice === 'string' && imagery.notice ? imagery.notice : GENERIC_NOTICE;
}

/** The `globe-info` line for a globe that is on (tiles streaming from `imagery`). */
export function globeOnInfo(imagery) {
  const attribution = imagery && imagery.attribution;
  return 'globe: Earth (quadtree WGS84, LOD by camera distance)' + (attribution ? ` · imagery: ${attribution}` : '');
}

/** The `globe-info` line when the globe was asked for but the basemap is not installed: the
 * plain body is kept and the notice says what to do. */
export function globeNotInstalledInfo(imagery) {
  return 'globe: not drawn, plain body kept. ' + basemapNotice(imagery);
}
