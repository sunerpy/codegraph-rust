function findItems() {}
function removeItem() {}

exports.getItems = async () => {
  findItems();
};

module.exports.deleteItem = function () {
  removeItem();
};

exports.plain = 42;
module.exports = { legacy: 1 };

const handlers = {};
handlers.onSave = () => findItems();
