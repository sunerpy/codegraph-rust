const express = require('express');

const router = express.Router();

function loadUser(id) {
  return { id };
}

router.get('/users/:id', (req, res) => {
  res.json(loadUser(req.params.id));
});

module.exports = router;
