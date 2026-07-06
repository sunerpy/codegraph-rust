require_relative "logger"

module Greeting
end

class Service
  include Greeting

  def initialize
    @logger = Logger.new
  end

  def run(message)
    @logger.log(message)
    Formatter.shout(message)
  end
end
