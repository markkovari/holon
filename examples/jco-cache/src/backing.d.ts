// Types for the fake backing-store shim (src/backing.js).
export const __backing: Map<string, Uint8Array>;
export function __seed(key: string, value: string): void;
export function load(key: string): Uint8Array | undefined;
export function store(key: string, value: Uint8Array): void;
export function remove(key: string): void;
