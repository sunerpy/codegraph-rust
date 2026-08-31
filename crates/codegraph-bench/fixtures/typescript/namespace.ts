function helper() {}

export const api = {
  run() {
    helper();
  },
  stop: () => helper(),
  nested: {
    run() {
      helper();
    },
  },
};

export function localCaller() {
  api.run();
  api.stop();
  api.nested.run();
}

export function run() {}
