// @ts-nocheck
// CHEX client for the web (browser JavaScript / TypeScript).
//
// Unlike the other clients, the browser can't spawn the `chex` binary — there is
// no subprocess, no filesystem, and no network surface. So this shim runs the
// CHEX validation rules *in-process*, against an in-memory schema object. No
// binary, no dependencies, no build step: drop it in and import it.
//
//   import { validate } from './chex.mjs'
//
//   const schema = { name: '^[A-Za-z]+ [A-Za-z]+$', age: '^[0-9]+$' }
//   const data = validate(schema, { name: 'Jane Doe', age: 30 })  // returns the data
//   // throws CHEXError on a schema mismatch
//
// `schema` is a plain object (the parsed contents of a *.schema.json file — in
// the browser you fetch/import it rather than read it from disk). Every leaf is
// a regex string; values are coerced to strings for matching, exactly as the
// `chex` binary does. This file is a faithful port of the binary's runtime
// validator and is kept in lockstep by a parity test (tests/unit/web-client).

export class CHEXError extends Error {}

const MAX_REGEX_LENGTH = 500;

const isNullableKey = (key) => key.endsWith('?');
const dataKeyOf = (key) => (isNullableKey(key) ? key.slice(0, -1) : key);
const isObject = (value) => typeof value === 'object' && value !== null && !Array.isArray(value);

// An object is a Record descriptor if its single key is a regex (starts with `^`).
const isRecordType = (schema) => {
  const keys = Object.keys(schema);
  return keys.length === 1 && keys[0].startsWith('^');
};

const testLeaf = (value, pattern, path) => {
  if (typeof pattern !== 'string' || pattern.length === 0) {
    throw new CHEXError(`Schema value for '${path}' must be a non-empty regex string`);
  }
  if (pattern.length > MAX_REGEX_LENGTH) {
    throw new CHEXError(`Regex pattern for '${path}' exceeds maximum allowed length`);
  }
  let regex;
  try {
    regex = new RegExp(pattern);
  } catch {
    throw new CHEXError(`Invalid RegEx pattern for '${path}'`);
  }
  if (!regex.test(String(value))) {
    throw new CHEXError(`RegEx pattern fails for property '${path}'`);
  }
};

const rejectMissing = (path) => {
  throw new CHEXError(`Property '${path}' cannot be null or undefined`);
};

const validateProperty = (schema, data, schemaKey, path) => {
  const schemaValue = schema[schemaKey];
  const nullable = isNullableKey(schemaKey);
  const dataKey = dataKeyOf(schemaKey);
  const value = data[dataKey];
  const fullPath = path ? `${path}.${dataKey}` : dataKey;
  const defined = value !== null && value !== undefined;

  if (typeof schemaValue === 'string') {
    if (!defined) return nullable ? undefined : rejectMissing(fullPath);
    testLeaf(value, schemaValue, fullPath);
    return;
  }

  if (Array.isArray(schemaValue)) {
    if (!defined) return nullable ? undefined : rejectMissing(fullPath);
    if (!Array.isArray(value)) throw new CHEXError(`Type mismatch for '${fullPath}': expected an array`);
    const item = schemaValue[0];
    if (typeof item === 'string') {
      for (const element of value) testLeaf(element, item, fullPath);
    } else if (isObject(item)) {
      value.forEach((element, index) => {
        if (!isObject(element)) throw new CHEXError(`Type mismatch for '${fullPath}[${index}]': expected an object`);
        walk(item, element, `${fullPath}[${index}]`);
      });
    }
    return;
  }

  if (isObject(schemaValue)) {
    if (!defined) return nullable ? undefined : rejectMissing(fullPath);
    if (!isObject(value)) throw new CHEXError(`Type mismatch for '${fullPath}': expected an object`);
    if (isRecordType(schemaValue)) {
      const keyPattern = Object.keys(schemaValue)[0];
      const valuePattern = schemaValue[keyPattern];
      for (const [k, v] of Object.entries(value)) {
        testLeaf(k, keyPattern, `${fullPath}.<key:${k}>`);
        testLeaf(v, valuePattern, `${fullPath}.${k}`);
      }
    } else {
      walk(schemaValue, value, fullPath);
    }
    return;
  }

  throw new CHEXError(`Schema value for '${fullPath}' must be a regex string`);
};

// Validate `data` against a schema object, recursively. Mirrors the binary's
// SchemaObjectValidator: reject unknown data keys, then check each schema key.
const walk = (schema, data, path) => {
  // Own enumerable keys only — never the prototype chain (avoids inherited/polluted keys).
  for (const dataKey of Object.keys(data)) {
    if (Object.hasOwn(schema, dataKey) || Object.hasOwn(schema, `${dataKey}?`)) continue;
    throw new CHEXError(`Property '${dataKey}' does not exist in schema`);
  }
  for (const schemaKey of Object.keys(schema)) {
    validateProperty(schema, data, schemaKey, path);
  }
  return data;
};

/**
 * Validate `data` against an in-memory CHEX schema object.
 * Returns the original data on success; throws CHEXError on the first mismatch.
 * @template {Record<string, unknown>} T
 * @param {Record<string, unknown>} schema
 * @param {T} data
 * @returns {T}
 */
export const validate = (schema, data) => {
  if (!isObject(schema)) throw new CHEXError('Schema must be a JSON object');
  if (Object.keys(schema).length === 0) throw new CHEXError('Schema must define at least one property');
  return walk(schema, data, '');
};

export default { validate, CHEXError };