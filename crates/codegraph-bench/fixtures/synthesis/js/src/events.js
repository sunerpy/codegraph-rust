const { EventEmitter } = require('events');

const bus = new EventEmitter();

function onSaved(record) {
  return record.id;
}

bus.on('saved', onSaved);

function save(record) {
  bus.emit('saved', record);
}

const handlers = {
  create: (payload) => payload,
  remove: (payload) => payload.id,
};

function dispatch(kind, payload) {
  return handlers[kind](payload);
}

module.exports = { save, dispatch };
