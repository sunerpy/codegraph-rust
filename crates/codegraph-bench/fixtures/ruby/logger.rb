class Logger
  def log(message)
    puts message
  end
end

class Formatter
  def self.shout(message)
    message.upcase
  end
end
