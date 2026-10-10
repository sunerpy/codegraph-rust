'use strict';

const request = require('supertest');
const createApp = require('../');
const { setCharset } = require('../lib/app');

describe('app', () => {
  it('builds', () => {
    request(createApp());
    setCharset('utf-8');
  });
});
