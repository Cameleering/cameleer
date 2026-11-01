import { helper, Box } from './util.js';
export const PI2 = 6.28;
export function square(x) { return x * x; }
export default function greet(n) { return 'hi ' + n; }
export const viaHelper = helper(2);
export { Box };
let counter = 0;
export function bump() { return ++counter; }
