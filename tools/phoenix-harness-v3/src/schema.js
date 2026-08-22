/**
 * Parameter-schema compiler (Lesson L-002).
 * The DeepSeek serializer forwards tool.parameters verbatim to the provider,
 * which requires an OBJECT-ROOT JSON Schema. ctx.tools.register only
 * validates output.schema. This minimal compiler turns the authoring spec
 * map {type, required, description, properties, items, additionalProperties}
 * into the exact object-root wire shape.
 * Pinned by tests/tool-schema.test.mjs + src/preflight/tool-schemas.mjs.
 */

const TYPE_SET = new Set(['string', 'number', 'integer', 'boolean', 'object', 'array', 'null'])

function assertSupported(spec, path) {
  if (spec === null || typeof spec !== 'object' || Array.isArray(spec)) {
    throw new Error(`${path}: schema must be a JSON Schema of type object`)
  }
  if (spec.type !== undefined && !TYPE_SET.has(spec.type)) {
    throw new Error(`${path}: unsupported type "${spec.type}"`)
  }
  if (spec.required === false) {
    throw new Error(`${path}: required:false is illegal at the provider boundary`)
  }
}

function compile(spec, path = '$') {
  assertSupported(spec, path)
  const out = {}
  if (spec.type !== undefined) out.type = spec.type
  if (spec.description !== undefined) out.description = String(spec.description)
  if (spec.type === 'object') {
    out.type = 'object'
    const props = {}
    const required = []
    for (const [k, v] of Object.entries(spec.properties ?? {})) {
      assertSupported(v, `${path}.${k}`)
      props[k] = compile(v, `${path}.${k}`)
      if (v.required === true) required.push(k)
    }
    out.properties = props
    if (required.length > 0) out.required = required
    if (spec.additionalProperties === false) out.additionalProperties = false
  } else if (spec.type === 'array') {
    out.type = 'array'
    if (spec.items !== undefined) {
      assertSupported(spec.items, `${path}.items`)
      out.items = compile(spec.items, `${path}.items`)
    }
  }
  if (spec.enum !== undefined && Array.isArray(spec.enum)) out.enum = spec.enum
  return out
}

/** Compile the authoring map into an object-root parameter schema. */
export function compileParameterSchema(map) {
  if (map === null || typeof map !== 'object' || Array.isArray(map)) {
    throw new Error('parameters: schema must be a JSON Schema of type object')
  }
  const out = { type: 'object', properties: {}, required: [] }
  for (const [name, spec] of Object.entries(map)) {
    assertSupported(spec, `$.${name}`)
    out.properties[name] = compile(spec, `$.${name}`)
    if (spec.required === true) out.required.push(name)
  }
  if (out.required.length === 0) delete out.required
  return out
}
