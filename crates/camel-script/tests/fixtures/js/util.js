export function helper(x) { return x * 10; }
export class Box { constructor(v) { this.v = v; } get double() { return this.v * 2; } }
export * from './consts.js';
