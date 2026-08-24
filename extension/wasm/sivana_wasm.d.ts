/* tslint:disable */
/* eslint-disable */
/**
*/
export class WasmFingerprinter {
  free(): void;
/**
* Create an engine for mono PCM at `sample_rate_hz` using the
* production operating point (E4: 512 log bands — MUST match the
* ingest configuration or hashes cannot collide).
* @param {number} sample_rate_hz
*/
  constructor(sample_rate_hz: number);
/**
* Push mono PCM; returns the SFP1 batch of fingerprints finalized
* by this chunk (may be empty).
* @param {Float32Array} pcm
* @returns {Uint8Array}
*/
  process(pcm: Float32Array): Uint8Array;
/**
* Flush end-of-stream state; returns any trailing batch.
* @returns {Uint8Array}
*/
  finish(): Uint8Array;
/**
* Human-readable engine identity for diagnostics panels.
* @returns {string}
*/
  version(): string;
}

export type InitInput = RequestInfo | URL | Response | BufferSource | WebAssembly.Module;

export interface InitOutput {
  readonly memory: WebAssembly.Memory;
  readonly __wbg_wasmfingerprinter_free: (a: number, b: number) => void;
  readonly wasmfingerprinter_new: (a: number) => number;
  readonly wasmfingerprinter_process: (a: number, b: number, c: number, d: number) => void;
  readonly wasmfingerprinter_finish: (a: number, b: number) => void;
  readonly wasmfingerprinter_version: (a: number, b: number) => void;
  readonly __wbindgen_add_to_stack_pointer: (a: number) => number;
  readonly __wbindgen_malloc: (a: number, b: number) => number;
  readonly __wbindgen_free: (a: number, b: number, c: number) => void;
}

export type SyncInitInput = BufferSource | WebAssembly.Module;
/**
* Instantiates the given `module`, which can either be bytes or
* a precompiled `WebAssembly.Module`.
*
* @param {{ module: SyncInitInput }} module - Passing `SyncInitInput` directly is deprecated.
*
* @returns {InitOutput}
*/
export function initSync(module: { module: SyncInitInput } | SyncInitInput): InitOutput;

/**
* If `module_or_path` is {RequestInfo} or {URL}, makes a request and
* for everything else, calls `WebAssembly.instantiate` directly.
*
* @param {{ module_or_path: InitInput | Promise<InitInput> }} module_or_path - Passing `InitInput` directly is deprecated.
*
* @returns {Promise<InitOutput>}
*/
export default function __wbg_init (module_or_path?: { module_or_path: InitInput | Promise<InitInput> } | InitInput | Promise<InitInput>): Promise<InitOutput>;
