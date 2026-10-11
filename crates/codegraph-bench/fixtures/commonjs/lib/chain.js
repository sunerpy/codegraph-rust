'use strict';

const wrap = require('./util').wrap;

module.exports = function chain(value) {
  return require('./format').title(wrap(value));
};
