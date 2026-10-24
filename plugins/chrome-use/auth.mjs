import { createHmac, timingSafeEqual } from 'node:crypto';

export const AUTH_VERSION = 2;
export const isAuthValue = value => typeof value === 'string' && /^[a-f0-9]{64}$/.test(value);

// Separate roles prevent reflection. JSON framing binds each field unambiguously;
// both random nonces make a captured proof unusable on a later connection.
export function authProof(token, role, { sessionId, clientNonce, serverNonce, taskKey }) {
  return createHmac('sha256', Buffer.from(token, 'hex'))
    .update(JSON.stringify(['openharness.chrome.auth', AUTH_VERSION, role, sessionId, clientNonce, serverNonce, taskKey ?? null]))
    .digest('hex');
}

export function verifyAuthProof(proof, token, role, context) {
  return isAuthValue(proof) && timingSafeEqual(Buffer.from(proof, 'hex'), Buffer.from(authProof(token, role, context), 'hex'));
}
