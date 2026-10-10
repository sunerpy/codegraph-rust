const express = require('express');
const users = require('./routes/users');

const app = express();

function requestLogger(req, res, next) {
  next();
}

app.use(requestLogger);
app.use('/api', users);
app.get('/health', function health(req, res) {
  res.json(status());
});

function status() {
  return { ok: true };
}

module.exports = app;
