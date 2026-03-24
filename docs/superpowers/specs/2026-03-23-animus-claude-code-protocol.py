"""
Animus ↔ Claude Code Protocol

Abstract interface for bidirectional communication between Animus daemon
and Claude Code running on Mac Studio.

Supports multiple transports:
- FileTransport: Drop .md files in shared directory (MVP, no deps)
- RedisTransport: Redis pub/sub (production, scales well)

Usage:
    transport = FileTransport(queue_dir="/tmp/animus-code-queue")
    transport.send_task(Task(id="123", action="analyze_github", params={...}))
    
    result = transport.recv_task(timeout=30)
    transport.send_result(result)
"""

import json
import uuid
import time
from pathlib import Path
from dataclasses import dataclass, asdict, field
from enum import Enum
from datetime import datetime
from typing import Optional, Dict, Any, List
from abc import ABC, abstractmethod
import logging

logger = logging.getLogger(__name__)


class TaskStatus(Enum):
    """Task lifecycle states."""
    PENDING = "pending"
    RUNNING = "running"
    COMPLETED = "completed"
    FAILED = "failed"
    TIMEOUT = "timeout"


@dataclass
class Task:
    """
    Message from Animus to Claude Code.
    
    Fields:
        id: Unique task ID (generated if not provided)
        action: What Claude Code should do (e.g., "analyze_github", "build_code", "run_tests")
        params: Action-specific parameters
        priority: Task priority (0-100, higher = sooner)
        timeout: Max execution time in seconds
        created_at: ISO timestamp
        status: Current task status
    """
    action: str
    params: Dict[str, Any] = field(default_factory=dict)
    id: str = field(default_factory=lambda: str(uuid.uuid4())[:8])
    priority: int = 50
    timeout: int = 300
    created_at: str = field(default_factory=lambda: datetime.now().isoformat())
    status: TaskStatus = TaskStatus.PENDING
    
    def to_json(self) -> str:
        """Serialize to JSON, handling enums."""
        data = asdict(self)
        data['status'] = self.status.value
        return json.dumps(data, indent=2)
    
    @classmethod
    def from_json(cls, json_str: str) -> "Task":
        """Deserialize from JSON."""
        data = json.loads(json_str)
        data['status'] = TaskStatus(data['status'])
        return cls(**data)


@dataclass
class Result:
    """
    Message from Claude Code back to Animus.
    
    Fields:
        task_id: ID of the task this result corresponds to
        status: Final task status
        output: Result data (action-specific)
        error: Error message if status is FAILED
        started_at: When execution began
        completed_at: When execution finished
    """
    task_id: str
    status: TaskStatus
    output: Optional[Dict[str, Any]] = None
    error: Optional[str] = None
    started_at: Optional[str] = None
    completed_at: Optional[str] = None
    
    def to_json(self) -> str:
        """Serialize to JSON."""
        data = asdict(self)
        data['status'] = self.status.value
        return json.dumps(data, indent=2)
    
    @classmethod
    def from_json(cls, json_str: str) -> "Result":
        """Deserialize from JSON."""
        data = json.loads(json_str)
        data['status'] = TaskStatus(data['status'])
        return cls(**data)


class Transport(ABC):
    """Abstract transport layer."""
    
    @abstractmethod
    def send_task(self, task: Task) -> None:
        """Send task to Claude Code."""
        pass
    
    @abstractmethod
    def recv_task(self, timeout: int = 30) -> Optional[Task]:
        """Receive task from Animus (Claude Code side)."""
        pass
    
    @abstractmethod
    def send_result(self, result: Result) -> None:
        """Send result back to Animus."""
        pass
    
    @abstractmethod
    def recv_result(self, task_id: str, timeout: int = 30) -> Optional[Result]:
        """Receive result from Claude Code (Animus side)."""
        pass
    
    @abstractmethod
    def health_check(self) -> bool:
        """Verify transport is operational."""
        pass


class FileTransport(Transport):
    """
    File-based transport using shared directory.
    
    Directory structure:
        queue_dir/
        ├── tasks/         (Animus writes, Claude Code reads)
        ├── results/       (Claude Code writes, Animus reads)
        └── processed/     (Completed tasks, archived)
    
    Files are named: {task_id}.json
    
    Advantages:
    - No external dependencies
    - Human-readable (easy to debug)
    - Works with filesystem permissions
    
    Disadvantages:
    - Polling-based (small overhead)
    - No built-in ordering guarantee
    - Not suitable for very high throughput
    """
    
    def __init__(self, queue_dir: str = "/tmp/animus-code-queue"):
        self.queue_dir = Path(queue_dir)
        self.tasks_dir = self.queue_dir / "tasks"
        self.results_dir = self.queue_dir / "results"
        self.processed_dir = self.queue_dir / "processed"
        
        # Create directory structure
        self.tasks_dir.mkdir(parents=True, exist_ok=True)
        self.results_dir.mkdir(parents=True, exist_ok=True)
        self.processed_dir.mkdir(parents=True, exist_ok=True)
        
        logger.info(f"FileTransport initialized: {queue_dir}")
    
    def send_task(self, task: Task) -> None:
        """Write task to tasks/ directory."""
        task_file = self.tasks_dir / f"{task.id}.json"
        task_file.write_text(task.to_json())
        logger.info(f"Task sent: {task.id} ({task.action})")
    
    def recv_task(self, timeout: int = 30) -> Optional[Task]:
        """
        Poll tasks/ directory for new tasks.
        Returns oldest pending task, blocks up to timeout seconds.
        """
        start_time = time.time()
        
        while time.time() - start_time < timeout:
            tasks = sorted(self.tasks_dir.glob("*.json"))
            
            if tasks:
                task_file = tasks[0]
                try:
                    task = Task.from_json(task_file.read_text())
                    logger.info(f"Task received: {task.id} ({task.action})")
                    return task
                except json.JSONDecodeError:
                    logger.error(f"Malformed task file: {task_file}")
                    task_file.unlink()  # Remove bad file
            
            time.sleep(0.5)  # Poll interval
        
        return None
    
    def send_result(self, result: Result) -> None:
        """Write result to results/ directory."""
        result_file = self.results_dir / f"{result.task_id}.json"
        result_file.write_text(result.to_json())
        logger.info(f"Result sent: {result.task_id} ({result.status.value})")
    
    def recv_result(self, task_id: str, timeout: int = 30) -> Optional[Result]:
        """
        Poll results/ directory for specific task result.
        Blocks up to timeout seconds.
        """
        start_time = time.time()
        result_file = self.results_dir / f"{task_id}.json"
        
        while time.time() - start_time < timeout:
            if result_file.exists():
                try:
                    result = Result.from_json(result_file.read_text())
                    logger.info(f"Result received: {task_id} ({result.status.value})")
                    
                    # Move to processed
                    processed_file = self.processed_dir / f"{task_id}.json"
                    result_file.rename(processed_file)
                    
                    return result
                except json.JSONDecodeError:
                    logger.error(f"Malformed result file: {result_file}")
                    result_file.unlink()
            
            time.sleep(0.5)
        
        logger.warning(f"Timeout waiting for result: {task_id}")
        return None
    
    def health_check(self) -> bool:
        """Verify directory structure is writable."""
        try:
            test_file = self.queue_dir / ".health_check"
            test_file.write_text(str(time.time()))
            test_file.unlink()
            return True
        except Exception as e:
            logger.error(f"Health check failed: {e}")
            return False


class RedisTransport(Transport):
    """
    Redis-based transport using pub/sub.
    
    Topics:
    - "animus:tasks" — Animus publishes tasks, Claude Code subscribes
    - "animus:results:{task_id}" — Claude Code publishes results
    
    Advantages:
    - Async, low latency
    - Persistent message queue option
    - Scales well
    - Language-agnostic
    
    Requires:
    - Redis server running (redis://localhost:6379)
    
    Usage:
        transport = RedisTransport(redis_url="redis://localhost:6379")
        transport.send_task(task)
    """
    
    def __init__(self, redis_url: str = "redis://localhost:6379"):
        try:
            import redis
            self.redis_url = redis_url
            self.client = redis.from_url(redis_url, decode_responses=True)
            self.client.ping()
            logger.info(f"RedisTransport initialized: {redis_url}")
        except ImportError:
            raise RuntimeError("redis package required. Install: pip install redis")
        except Exception as e:
            raise RuntimeError(f"Redis connection failed: {e}")
    
    def send_task(self, task: Task) -> None:
        """Publish task to animus:tasks channel."""
        self.client.publish("animus:tasks", task.to_json())
        logger.info(f"Task published: {task.id} ({task.action})")
    
    def recv_task(self, timeout: int = 30) -> Optional[Task]:
        """Subscribe and receive next task (blocking)."""
        pubsub = self.client.pubsub()
        pubsub.subscribe("animus:tasks")
        
        try:
            message = pubsub.get_message(ignore_subscribe_messages=True, timeout=timeout)
            if message:
                task = Task.from_json(message['data'])
                logger.info(f"Task received: {task.id} ({task.action})")
                return task
        finally:
            pubsub.close()
        
        return None
    
    def send_result(self, result: Result) -> None:
        """Publish result to animus:results:{task_id}."""
        channel = f"animus:results:{result.task_id}"
        self.client.publish(channel, result.to_json())
        logger.info(f"Result published: {result.task_id} ({result.status.value})")
    
    def recv_result(self, task_id: str, timeout: int = 30) -> Optional[Result]:
        """Subscribe to result channel and wait for response."""
        pubsub = self.client.pubsub()
        channel = f"animus:results:{task_id}"
        pubsub.subscribe(channel)
        
        try:
            message = pubsub.get_message(ignore_subscribe_messages=True, timeout=timeout)
            if message:
                result = Result.from_json(message['data'])
                logger.info(f"Result received: {task_id} ({result.status.value})")
                return result
        finally:
            pubsub.close()
        
        logger.warning(f"Timeout waiting for result: {task_id}")
        return None
    
    def health_check(self) -> bool:
        """Ping Redis."""
        try:
            self.client.ping()
            return True
        except Exception as e:
            logger.error(f"Redis health check failed: {e}")
            return False


class HybridTransport(Transport):
    """
    Hybrid transport: uses FileTransport by default, Redis when available.
    
    Allows graceful fallback and gradual migration from file-based to Redis.
    
    Configuration:
    - Try Redis first
    - If Redis unavailable, use FileTransport
    - On each send/recv, check health and switch if needed
    """
    
    def __init__(
        self,
        redis_url: str = "redis://localhost:6379",
        queue_dir: str = "/tmp/animus-code-queue",
        force_file: bool = False
    ):
        self.redis_url = redis_url
        self.queue_dir = queue_dir
        self.force_file = force_file
        
        self.redis_transport = None
        self.file_transport = FileTransport(queue_dir)
        
        if not force_file:
            try:
                self.redis_transport = RedisTransport(redis_url)
                logger.info("HybridTransport: Redis available, using as primary")
            except Exception as e:
                logger.warning(f"HybridTransport: Redis unavailable ({e}), using FileTransport")
    
    def _get_transport(self) -> Transport:
        """Get active transport (Redis if healthy, else File)."""
        if self.redis_transport and self.redis_transport.health_check():
            return self.redis_transport
        return self.file_transport
    
    def send_task(self, task: Task) -> None:
        self._get_transport().send_task(task)
    
    def recv_task(self, timeout: int = 30) -> Optional[Task]:
        return self._get_transport().recv_task(timeout)
    
    def send_result(self, result: Result) -> None:
        self._get_transport().send_result(result)
    
    def recv_result(self, task_id: str, timeout: int = 30) -> Optional[Result]:
        return self._get_transport().recv_result(task_id, timeout)
    
    def health_check(self) -> bool:
        return self._get_transport().health_check()


# Convenience factory
def create_transport(
    mode: str = "hybrid",
    redis_url: str = "redis://localhost:6379",
    queue_dir: str = "/tmp/animus-code-queue"
) -> Transport:
    """
    Factory function to create appropriate transport.
    
    Args:
        mode: "file", "redis", or "hybrid"
        redis_url: Redis connection string (ignored if mode="file")
        queue_dir: Shared directory (ignored if mode="redis")
    
    Returns:
        Transport instance
    """
    if mode == "file":
        return FileTransport(queue_dir)
    elif mode == "redis":
        return RedisTransport(redis_url)
    elif mode == "hybrid":
        return HybridTransport(redis_url, queue_dir)
    else:
        raise ValueError(f"Unknown transport mode: {mode}")
