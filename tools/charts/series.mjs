/**
 * The project's series identity, in one place.
 *
 * `bencharts` deliberately knows nothing about tiles, engines or backends: a
 * library that knows what a tile is has failed at being extracted. So the
 * vocabulary lives here, in the adapter, which is the only layer entitled to
 * it.
 *
 * Colours are declared rather than left to the fallback palette, because a
 * declared series keeps the same colour in every chart while a fallback is only
 * stable for a given set of undeclared series.
 */

import { defineSeries } from 'bencharts';

/**
 * Draw order matters. Putting blue between green and red is what keeps the
 * green and the red apart in a bar group, which is the pair a reader compares.
 * Each engine keeps the hue family it has in the published articles.
 */
export const TECHNOLOGIES = Object.freeze([
  { key: 'libvips', label: 'libvips', color: '#ab47bc' },
  { key: 'streaming', label: 'Streaming', color: '#34a853' },
  { key: 'monolithic', label: 'Monolithic', color: '#2196f3' },
  { key: 'mapreduce', label: 'MapReduce', color: '#c62828' },
  { key: 'directory', label: 'Directory', color: '#7986cb' },
  { key: 'pmtiles', label: 'PMTiles', color: '#e65100' },
]);

export const series = defineSeries(TECHNOLOGIES);

/** Human labels for the metric ids the harness emits. */
export const METRIC_LABELS = Object.freeze({
  wall: 'Wall time',
  peak_rss_mb: 'Peak RSS',
  tracked_memory_mb: 'Tracked working set',
  tiles_per_second: 'Throughput',
  tiles_per_second_per_mb: 'Throughput per MB',
  resource_cost: 'Resource cost',
  p50: 'Median latency',
  p99: 'p99 latency',
  max: 'Worst latency',
  tiles_per_s: 'Throughput',
  lookups_per_s: 'Lookup rate',
  ns_per_entry: 'Cost per entry',
  requests: 'Requests',
  requests_declared: 'Requests (declared)',
  request_bytes: 'Request bytes',
  request_bytes_declared: 'Request bytes (declared)',
});

export const labelFor = (metric) => METRIC_LABELS[metric] ?? metric;

/** `direction` as the harness spells it, as `better` as bencharts spells it. */
export const betterOf = (direction) =>
  (direction === 'higher-is-better' ? 'higher' : direction === 'lower-is-better' ? 'lower' : undefined);
