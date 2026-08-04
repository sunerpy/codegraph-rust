function constTarget() {}

export const constAlias = constTarget;

function namedTarget() {}

export { namedTarget as namedAlias };

function defaultTarget() {}

export default defaultTarget;
