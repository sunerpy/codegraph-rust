'use strict';

const { default: Service } = require('./service');
const util = require('./util'),
  format = require('./format');

function createApp() {
  const service = new Service();
  return util.wrap(format.title(service.name()));
}

module.exports = createApp;
module.exports.setCharset = function setCharset(value) {
  return format.title(value);
};
